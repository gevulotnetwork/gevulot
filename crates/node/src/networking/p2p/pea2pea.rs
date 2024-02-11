use crate::txvalidation::P2pSender;
use crate::txvalidation::TxEventSender;
use futures_util::Stream;
use std::{
    collections::{BTreeSet, HashMap},
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    str,
    sync::Arc,
};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::pin;
use tokio_stream::StreamExt;

use super::{noise, protocol};
use bytes::{Bytes, BytesMut};
use gevulot_node::types::{
    transaction::{TxCreate, TxValdiated},
    Transaction,
};
use parking_lot::RwLock;
use pea2pea::{
    protocols::{Handshake, OnDisconnect, Reading, Writing},
    Config, Connection, ConnectionSide, Node, Pea2Pea,
};
use sha3::{Digest, Sha3_256};

// #[async_trait::async_trait]
// pub trait TxHandler: Send + Sync {
//     async fn recv_tx(&self, tx: Transaction) -> Result<()>;
// }

// #[async_trait::async_trait]
// pub trait TxChannel: Send + Sync {
//     async fn send_tx(&self, tx: &Transaction) -> Result<()>;
// }

// struct BlackholeTxHandler;
// #[async_trait::async_trait]
// impl TxHandler for BlackholeTxHandler {
//     async fn recv_tx(&self, tx: Transaction) -> Result<()> {
//         tracing::debug!("submitting received tx to black hole");
//         Ok(())
//     }
// }

// NOTE: This P2P implementation is originally from `pea2pea` Noise handshake example.
#[derive(Clone)]
pub struct P2P {
    node: Node,
    noise_states: Arc<RwLock<HashMap<SocketAddr, noise::State>>>,
    //tx_handler: Arc<tokio::sync::RwLock<Arc<dyn TxHandler>>>,

    // Peer connection map: <(P2P TCP connection's peer address) , (peer's advertised address in peer_list)>.
    // This mapping is needed for proper cleanup on OnDisconnect.
    peer_addr_mapping: Arc<tokio::sync::RwLock<HashMap<SocketAddr, SocketAddr>>>,
    peer_list: Arc<tokio::sync::RwLock<BTreeSet<SocketAddr>>>,
    //contains corrected peers use for asset file download.
    pub peer_http_port_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,

    http_port: Option<u16>,
    nat_listen_addr: Option<SocketAddr>,
    psk: Vec<u8>,

    //send Tx to the process loop
    tx_sender: TxEventSender<P2pSender>,
}

impl Pea2Pea for P2P {
    fn node(&self) -> &Node {
        &self.node
    }
}

impl P2P {
    pub async fn new(
        name: &str,
        listen_addr: SocketAddr,
        psk_passphrase: &str,
        http_port: Option<u16>,
        nat_listen_addr: Option<SocketAddr>,
        peer_http_port_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
        tx_sender: TxEventSender<P2pSender>,
        propagate_tx_stream: impl Stream<Item = Transaction<TxValdiated>> + std::marker::Send + 'static,
    ) -> Self {
        let config = Config {
            name: Some(name.into()),
            listener_ip: Some(listen_addr.ip()),
            desired_listening_port: Some(listen_addr.port()),
            ..Default::default()
        };
        let node = Node::new(config);

        // Main purpose of hashing here is to convert any passphrase string
        // into a 32 bytes long "string".
        let mut hasher = Sha3_256::new();
        hasher.update(psk_passphrase);
        let psk = hasher.finalize();

        let instance = Self {
            node,
            noise_states: Default::default(),
            //tx_handler: Arc::new(tokio::sync::RwLock::new(Arc::new(BlackholeTxHandler {}))),
            psk: psk.to_vec(),
            peer_list: Default::default(),
            peer_addr_mapping: Default::default(),
            peer_http_port_list,
            http_port,
            nat_listen_addr,
            tx_sender,
        };

        // Enable node functionalities.
        instance.enable_handshake().await;
        instance.enable_reading().await;
        instance.enable_writing().await;
        instance.enable_disconnect().await;

        //start new Tx stream loop
        tokio::spawn({
            let p2p = instance.clone();
            async move {
                pin!(propagate_tx_stream);
                while let Some(tx) = propagate_tx_stream.next().await {
                    let tx_hash = tx.hash;
                    let msg = protocol::Message::V0(protocol::MessageV0::Transaction(tx));
                    let bs = match bincode::serialize(&msg) {
                        Ok(bs) => bs,
                        Err(err) => {
                            tracing::error!(
                                "Tx:{tx_hash} not send because serialization fail:{err}",
                            );
                            continue;
                        }
                    };
                    let bs = Bytes::from(bs);
                    tracing::debug!("broadcasting transaction {}", tx_hash);
                    if let Err(err) = p2p.broadcast(bs) {
                        tracing::error!("Tx:{tx_hash} not send because :{err}");
                    }
                }
            }
        });

        instance
    }

    // pub async fn register_tx_handler(&self, tx_handler: Arc<dyn TxHandler>) {
    //     let mut old_handler = self.tx_handler.write().await;
    //     *old_handler = tx_handler;
    //     tracing::debug!("new tx handler registered");
    // }

    async fn recv_tx(&self, tx: Transaction<TxCreate>) {
        tracing::debug!("submitting received tx to tx_handler");
        if let Err(err) = self.tx_sender.send_tx(tx) {
            tracing::error!("P2P error during received Tx sending:{err}");
        }

        // let tx_handler = self.tx_handler.read().await;
        // if let Err(err) = tx_handler.recv_tx(tx).await {
        //     tracing::error!("failed to handle incoming transaction: {}", err);
        // } else {
        //     tracing::debug!("submitted received tx to tx_handler");
        // }
    }

    async fn build_handshake_msg(&self) -> protocol::Handshake {
        let my_local_bind_addr = self.node.listening_addr().expect("p2p node listening_addr");

        // If NAT listen address hasn't been set, default 0.0.0.0:<port> is
        // used as a placeholder and replaced with the effective remote address
        // observed by peer.
        let my_p2p_listen_addr = self.nat_listen_addr.unwrap_or(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)),
            my_local_bind_addr.port(),
        ));

        let peers: BTreeSet<SocketAddr> = {
            let mut peer_list = self.peer_list.write().await;
            // Ensure that our local address is present.
            (*peer_list).insert(my_p2p_listen_addr);
            peer_list.clone()
        };

        protocol::Handshake::V1(protocol::HandshakeV1 {
            my_p2p_listen_addr,
            peers,
            http_port: self.http_port,
        })
    }

    async fn process_diagnostics_request(
        &self,
        source: SocketAddr,
        req: protocol::DiagnosticsRequestKind,
    ) -> io::Result<()> {
        let resp = protocol::DiagnosticsResponseV0::Version {
            major: env!("CARGO_PKG_VERSION_MAJOR").parse::<u16>().unwrap(),
            minor: env!("CARGO_PKG_VERSION_MINOR").parse::<u16>().unwrap(),
            patch: env!("CARGO_PKG_VERSION_PATCH").parse::<u16>().unwrap(),
            build: format!(
                "{}: {}",
                env!("VERGEN_BUILD_TIMESTAMP"),
                env!("VERGEN_GIT_DESCRIBE")
            ),
        };

        let bs =
            Bytes::from(bincode::serialize(&resp).expect("diagnostics response serialization"));

        // Reply to requester.
        self.unicast(source, bs)?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl Handshake for P2P {
    async fn perform_handshake(&self, mut conn: Connection) -> io::Result<Connection> {
        tracing::debug!("starting handshake");

        // Create the noise objects.
        let noise_builder =
            snow::Builder::new("Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s".parse().unwrap());
        let noise_keypair = noise_builder.generate_keypair().unwrap();
        let noise_builder = noise_builder.local_private_key(&noise_keypair.private);
        let noise_builder = noise_builder.psk(3, self.psk.as_slice());

        // Perform the noise handshake.
        let (noise_state, _) =
            noise::handshake_xx(self, &mut conn, noise_builder, Bytes::new()).await?;

        // Save the noise state to be reused by Reading and Writing.
        self.noise_states.write().insert(conn.addr(), noise_state);

        tracing::debug!("noise handshake finished. exchanging node information");

        // Exchange application level handshake message.
        let node_conn_side = !conn.side();
        let stream = self.borrow_stream(&mut conn);

        let peer_handshake_msg: protocol::Handshake = match node_conn_side {
            ConnectionSide::Initiator => {
                // Serialize & send our handshake message.
                let handshake_msg_bytes = bincode::serialize(&self.build_handshake_msg().await)
                    .map_err(|err| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("serialize error:{err}"),
                        )
                    })?;
                stream.write_u32(handshake_msg_bytes.len() as u32).await?;
                stream.write_all(&handshake_msg_bytes).await?;

                // Receive handshake message from peer.
                let buffer_len = stream.read_u32().await? as usize;

                // TODO: Validate buffer length.
                let mut buffer = vec![0; buffer_len];
                stream.read_exact(&mut buffer).await?;

                bincode::deserialize(&buffer).map_err(|err| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("deserialize error:{err}"),
                    )
                })?
            }
            ConnectionSide::Responder => {
                // Receive the handshake message from the connecting peer.
                let buffer_len = stream.read_u32().await? as usize;
                let mut buffer = vec![0; buffer_len];
                stream.read_exact(&mut buffer).await?;

                let peer_handshake_msg: protocol::Handshake = bincode::deserialize(&buffer)
                    .map_err(|err| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("deserialize error:{err}"),
                        )
                    })?;

                // Serialize & send our handshake message.
                let handshake_msg_bytes = bincode::serialize(&self.build_handshake_msg().await)
                    .map_err(|err| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("serialize error:{err}"),
                        )
                    })?;
                stream.write_u32(handshake_msg_bytes.len() as u32).await?;
                stream.write_all(&handshake_msg_bytes).await?;

                peer_handshake_msg
            }
        };

        #[allow(clippy::infallible_destructuring_match)]
        let mut handshake_msg = match peer_handshake_msg {
            protocol::Handshake::V1(msg) => msg,
        };

        // Current TCP connection peer address.
        let remote_peer = stream.peer_addr().unwrap();

        // Check if the remote P2P listen address needs to be updated from
        // the one observed in connection.
        let default_ip = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
        if handshake_msg.my_p2p_listen_addr.ip() == default_ip {
            handshake_msg
                .peers
                .remove(&handshake_msg.my_p2p_listen_addr);
            handshake_msg.my_p2p_listen_addr =
                SocketAddr::new(remote_peer.ip(), handshake_msg.my_p2p_listen_addr.port());
            handshake_msg.peers.insert(handshake_msg.my_p2p_listen_addr);
        }

        tracing::debug!("node information exchanged.");

        tracing::debug!("tcp connection peer address: {}", remote_peer);
        tracing::debug!(
            "peer advertised address: {}",
            handshake_msg.my_p2p_listen_addr
        );

        if tracing::enabled!(tracing::Level::DEBUG) {
            let print_peers: Vec<String> =
                handshake_msg.peers.iter().map(|x| x.to_string()).collect();
            tracing::debug!("peer contact list addresses: {:#?}", print_peers);
        }

        // Advertised remote peer listen address.
        let remote_peer_p2p_addr = &handshake_msg.my_p2p_listen_addr;

        tracing::debug!(
            "new connection: local:{} peer:{}",
            self.node.listening_addr().unwrap(), // Cannot fail.
            remote_peer
        );

        // Insert mapping between current TCP connection peer address and
        // the advertised remote listen address (the one present in peer list).
        self.peer_addr_mapping
            .write()
            .await
            .insert(remote_peer, *remote_peer_p2p_addr);

        // Capture the broadcasted public P2P listening addresses. These can be
        // different than the actual bind addresses (e.g. w/ port forwarding).
        let local_p2p_addr = self
            .nat_listen_addr
            .unwrap_or(self.node.listening_addr().unwrap());

        // Merge remote peer list with the local one to get full view on the network.
        let mut local_diff = {
            let mut local_peer_list = self.peer_list.write().await;
            let local_diff: BTreeSet<SocketAddr> = handshake_msg
                .peers
                .difference(&*local_peer_list)
                .cloned()
                .collect();

            (*local_peer_list).append(&mut local_diff.iter().cloned().collect());
            local_diff
        };
        local_diff.remove(&local_p2p_addr);
        local_diff.remove(remote_peer_p2p_addr);

        tracing::debug!("found {} new nodes", local_diff.len());
        let node = self.node();
        for addr in local_diff {
            tracing::debug!("connect to {}", &addr);

            // XXX: If `node.connect(addr)` returns an error, it's omitted because:
            // 1.) It's already logged.
            // 2.) It often happens because there is already a connection between the 2 peers.
            let _ = node.connect(addr).await;
        }

        self.peer_http_port_list
            .write()
            .await
            .insert(handshake_msg.my_p2p_listen_addr, handshake_msg.http_port);

        Ok(conn)
    }
}

#[async_trait::async_trait]
impl Reading for P2P {
    type Message = BytesMut;
    type Codec = noise::Codec;

    fn codec(&self, addr: SocketAddr, _side: ConnectionSide) -> Self::Codec {
        let state = self.noise_states.read().get(&addr).cloned().unwrap();
        noise::Codec::new(2, u16::MAX as usize, state, self.node().span().clone())
    }

    async fn process_message(&self, source: SocketAddr, message: Self::Message) -> io::Result<()> {
        tracing::debug!(parent: self.node().span(), "decrypted a message from {}", source);

        match bincode::deserialize(message.as_ref()) {
            Ok(protocol::Message::V0(msg)) => match msg {
                protocol::MessageV0::Transaction(tx) => {
                    tracing::debug!(
                        "received transaction {} author:{}",
                        tx.hash,
                        hex::encode(tx.author.serialize())
                    );
                    let tx: Transaction<TxCreate> = Transaction {
                        author: tx.author,
                        hash: tx.hash,
                        payload: tx.payload,
                        nonce: tx.nonce,
                        signature: tx.signature,
                        propagated: tx.propagated,
                        executed: tx.executed,
                        state: TxCreate,
                    };
                    self.recv_tx(tx).await;
                }
                protocol::MessageV0::DiagnosticsRequest(kind) => {
                    tracing::debug!("received diagnostics request");
                    self.process_diagnostics_request(source, kind).await?;
                }
                // Nodes are expected to ignore the diagnostics response.
                protocol::MessageV0::DiagnosticsResponse(_) => (),
            },
            Err(err) => tracing::error!("failed to decode incoming transaction: {}", err),
        }

        Ok(())
    }
}

// #[async_trait::async_trait]
// impl TxChannel for P2P {
//     async fn send_tx(&self, tx: &Transaction) -> Result<()> {
//         let msg = protocol::Message::V0(protocol::MessageV0::Transaction(tx.clone()));
//         let bs = Bytes::from(bincode::serialize(&msg)?);

//         tracing::debug!("broadcasting transaction {}", tx.hash);
//         self.broadcast(bs)?;
//         Ok(())
//     }
// }
impl Writing for P2P {
    type Message = Bytes;
    type Codec = noise::Codec;

    fn codec(&self, addr: SocketAddr, _side: ConnectionSide) -> Self::Codec {
        let state = self.noise_states.write().remove(&addr).unwrap();
        noise::Codec::new(2, u16::MAX as usize, state, self.node().span().clone())
    }
}

#[async_trait::async_trait]
impl OnDisconnect for P2P {
    async fn on_disconnect(&self, addr: SocketAddr) {
        if let Some(peer_conn_addr) = self.peer_addr_mapping.write().await.remove(&addr) {
            let _ = self.peer_list.write().await.remove(&peer_conn_addr);
            self.peer_http_port_list
                .write()
                .await
                .remove(&peer_conn_addr);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eyre::Result;
    use gevulot_node::types::{transaction::Payload, Hash, Transaction};
    use libsecp256k1::SecretKey;
    use rand::{rngs::StdRng, RngCore, SeedableRng};
    use tokio::sync::mpsc::{self, Sender};
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::EnvFilter;

    struct Sink(Arc<Sender<Transaction>>);
    impl Sink {
        fn new(tx: Arc<Sender<Transaction>>) -> Self {
            Self(tx)
        }
    }

    #[async_trait::async_trait]
    impl TxHandler for Sink {
        async fn recv_tx(&self, tx: Transaction) -> Result<()> {
            tracing::debug!("sink received new transaction");
            self.0.send(tx).await.expect("sink send");
            tracing::debug!("sink submitted tx to channel");
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_peer_list_inter_connection() {
        //start_logger(LevelFilter::ERROR);

        let (tx1, mut rx1) = mpsc::channel(1);
        let (tx2, mut rx2) = mpsc::channel(1);
        let (tx3, mut rx3) = mpsc::channel(1);
        let (sink1, sink2, sink3) = (
            Arc::new(Sink::new(Arc::new(tx1))),
            Arc::new(Sink::new(Arc::new(tx2))),
            Arc::new(Sink::new(Arc::new(tx3))),
        );
        let http_peer_list1: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>> =
            Default::default();
        let (_, p2p_recv1) = mpsc::unbounded_channel::<Transaction>();
        let p2p_stream1 = UnboundedReceiverStream::new(p2p_recv);

        let http_peer_list2: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>> =
            Default::default();
        let (_, p2p_recv2) = mpsc::unbounded_channel::<Transaction>();
        let p2p_stream2 = UnboundedReceiverStream::new(p2p_recv);

        let http_peer_list3: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>> =
            Default::default();
        let (_, p2p_recv3) = mpsc::unbounded_channel::<Transaction>();
        let p2p_stream3 = UnboundedReceiverStream::new(p2p_recv);

        let (peer1, peer2, peer3) = (
            P2P::new(
                "peer1",
                "127.0.0.1:0".parse().unwrap(),
                "secret passphrase",
                None,
                None,
                http_peer_list1,
                p2p_recv1,
                p2p_stream1,
            )
            .await,
            P2P::new(
                "peer2",
                "127.0.0.1:0".parse().unwrap(),
                "secret passphrase",
                Some(9995),
                None,
                http_peer_list2,
                p2p_recv2,
                p2p_stream2,
            )
            .await,
            P2P::new(
                "peer3",
                "127.0.0.1:0".parse().unwrap(),
                "secret passphrase",
                Some(9995),
                None,
                http_peer_list3,
                p2p_recv3,
                p2p_stream3,
            )
            .await,
        );

        tracing::debug!("start listening");
        let bind_add = peer1.node().start_listening().await.expect("peer1 listen");
        let bind_add = peer2.node().start_listening().await.expect("peer2 listen");
        let bind_add = peer3.node().start_listening().await.expect("peer3 listen");

        tracing::debug!("register tx handlers");
        peer1.register_tx_handler(sink1.clone()).await;
        peer2.register_tx_handler(sink2.clone()).await;
        peer3.register_tx_handler(sink3.clone()).await;

        tracing::debug!("connect peer2 to peer1");
        peer2
            .node()
            .connect(peer1.node().listening_addr().unwrap())
            .await
            .unwrap();

        assert_eq!(peer1.peer_http_port_list.read().await.len(), 1);
        assert_eq!(peer2.peer_http_port_list.read().await.len(), 1);

        tracing::debug!("connect peer3 to peer1");
        peer3
            .node()
            .connect(peer1.node().listening_addr().unwrap())
            .await
            .unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        assert_eq!(peer1.peer_http_port_list.read().await.len(), 2);
        assert_eq!(peer2.peer_http_port_list.read().await.len(), 2);
        assert_eq!(peer3.peer_http_port_list.read().await.len(), 2);

        tracing::debug!("send tx from peer2 to peer1 and peer3");
        let tx = new_tx();
        peer2.send_tx(&tx).await.unwrap();
        tracing::debug!("recv tx on peer1 from peer2");
        let recv_tx = rx1.recv().await.expect("sink recv");
        assert_eq!(tx, recv_tx);
        tracing::debug!("recv tx on peer3 from peer2");
        let recv_tx = rx3.recv().await.expect("sink recv");
        assert_eq!(tx, recv_tx);

        let tx = new_tx();
        tracing::debug!("send tx from peer3 to peer1 and peer2");
        peer3.send_tx(&tx).await.unwrap();
        tracing::debug!("recv tx on peer1 from peer3");
        let recv_tx = rx1.recv().await.expect("sink recv");
        assert_eq!(tx, recv_tx);
        tracing::debug!("recv tx on peer2 from peer3");
        let recv_tx = rx2.recv().await.expect("sink recv");
        assert_eq!(tx, recv_tx);
    }

    #[tokio::test]
    async fn test_two_peers_disconnect() {
        //start_logger(LevelFilter::ERROR);

        let (tx1, mut rx1) = mpsc::channel(1);
        let (tx2, mut rx2) = mpsc::channel(1);
        let (sink1, sink2) = (
            Arc::new(Sink::new(Arc::new(tx1))),
            Arc::new(Sink::new(Arc::new(tx2))),
        );
        let http_peer_list1: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>> =
            Default::default();
        let (_, p2p_recv1) = mpsc::unbounded_channel::<Transaction>();
        let p2p_stream1 = UnboundedReceiverStream::new(p2p_recv);

        let http_peer_list2: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>> =
            Default::default();
        let (_, p2p_recv2) = mpsc::unbounded_channel::<Transaction>();
        let p2p_stream2 = UnboundedReceiverStream::new(p2p_recv);

        let peer1 = P2P::new(
            "peer1",
            "127.0.0.1:0".parse().unwrap(),
            "secret passphrase",
            None,
            None,
            http_peer_list1,
            http_peer_list1,
            p2p_recv1,
        )
        .await;
        peer1.node().start_listening().await.expect("peer1 listen");
        peer1.register_tx_handler(sink1.clone()).await;

        {
            let peer2 = P2P::new(
                "peer2",
                "127.0.0.1:0".parse().unwrap(),
                "secret passphrase",
                Some(8776),
                None,
                http_peer_list2,
                http_peer_list2,
                p2p_recv2,
            )
            .await;
            peer2.node().start_listening().await.expect("peer2 listen");
            peer2.register_tx_handler(sink2.clone()).await;

            tracing::debug!("Nodes init Done");

            peer1
                .node()
                .connect(peer2.node().listening_addr().unwrap())
                .await
                .unwrap();
            assert_eq!(peer1.peer_http_port_list.read().await.len(), 1);
            assert_eq!(peer2.peer_http_port_list.read().await.len(), 1);

            tracing::debug!("Nodes Connected");
            tracing::debug!("send tx from peer1 to peer2");
            let tx = new_tx();
            peer1.send_tx(&tx).await.unwrap();
            tracing::debug!("recv tx on peer2 from peer1");
            let recv_tx = rx2.recv().await.expect("sink recv");
            assert_eq!(tx, recv_tx);

            let tx = new_tx();
            tracing::debug!("send tx from peer2 to peer1");
            peer2.send_tx(&tx).await.unwrap();
            tracing::debug!("recv tx on peer1 from peer2");
            let recv_tx = rx1.recv().await.expect("sink recv");
            assert_eq!(tx, recv_tx);

            let peers = peer2.node().connected_addrs();
            for addr in peers {
                peer2.node().disconnect(addr).await;
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Simulate the silent node disconnection by dropping the node.
        tracing::debug!("send tx from peer1 to disconnected peer2");
        let tx = new_tx();
        peer1.send_tx(&tx).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        assert_eq!(peer1.peer_list.read().await.len(), 1);
        assert!(peer1.peer_addr_mapping.read().await.is_empty());
        assert_eq!(peer1.peer_http_port_list.read().await.len(), 0);
        assert_eq!(peer1.peer_http_port_list.read().await.len(), 0);
    }

    #[tokio::test]
    async fn test_two_peers() {
        //start_logger(LevelFilter::ERROR);

        let (tx1, mut rx1) = mpsc::channel(1);
        let (tx2, mut rx2) = mpsc::channel(1);
        let (sink1, sink2) = (
            Arc::new(Sink::new(Arc::new(tx1))),
            Arc::new(Sink::new(Arc::new(tx2))),
        );
        let (peer1, peer2) = (
            P2P::new(
                "peer1",
                "127.0.0.1:0".parse().unwrap(),
                "secret passphrase",
                None,
                None,
            )
            .await,
            P2P::new(
                "peer2",
                "127.0.0.1:0".parse().unwrap(),
                "secret passphrase",
                None,
                None,
            )
            .await,
        );

        tracing::debug!("start listening");
        peer1.node().start_listening().await.expect("peer1 listen");
        peer2.node().start_listening().await.expect("peer2 listen");

        tracing::debug!("register tx handlers");
        peer1.register_tx_handler(sink1.clone()).await;
        peer2.register_tx_handler(sink2.clone()).await;

        tracing::debug!("connect peer2 to peer1");
        peer2
            .node()
            .connect(peer1.node().listening_addr().unwrap())
            .await
            .unwrap();

        tracing::debug!("send tx from peer1 to peer2");
        let tx = new_tx();
        peer1.send_tx(&tx).await.unwrap();
        tracing::debug!("recv tx on peer2 from peer1");
        let recv_tx = rx2.recv().await.expect("sink recv");
        assert_eq!(tx, recv_tx);

        let tx = new_tx();
        tracing::debug!("send tx from peer2 to peer1");
        peer2.send_tx(&tx).await.unwrap();
        tracing::debug!("recv tx on peer1 from peer2");
        let recv_tx = rx1.recv().await.expect("sink recv");
        assert_eq!(tx, recv_tx);
    }

    fn new_tx() -> Transaction {
        let rng = &mut StdRng::from_entropy();
        let mut tx = Transaction {
            hash: Hash::random(rng),
            payload: Payload::Empty,
            nonce: rng.next_u64(),
            ..Default::default()
        };

        let key = SecretKey::random(rng);
        tx.sign(&key);
        tx
    }

    fn start_logger(default_level: LevelFilter) {
        let filter = match EnvFilter::try_from_default_env() {
            Ok(filter) => filter.add_directive("tokio_util=off".parse().unwrap()),
            _ => EnvFilter::default()
                .add_directive(default_level.into())
                .add_directive("tokio_util=off".parse().unwrap()),
        };

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .without_time()
            .with_target(false)
            .init();
    }
}
