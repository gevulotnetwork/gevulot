use crate::metrics;
use crate::txvalidation::P2pSender;
use crate::txvalidation::TxEventSender;
use futures_util::Stream;
use libsecp256k1::PublicKey;
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
    transaction::{Created, Validated},
    Transaction,
};
use parking_lot::RwLock;
use pea2pea::{
    protocols::{Handshake, OnDisconnect, Reading, Writing},
    Config, Connection, ConnectionSide, Node, Pea2Pea,
};
use sha3::{Digest, Sha3_256};

// NOTE: This P2P implementation is originally from `pea2pea` Noise handshake example.
#[derive(Clone)]
pub struct P2P {
    node: Node,
    noise_states: Arc<RwLock<HashMap<SocketAddr, noise::State>>>,

    // Peer connection map: <(P2P TCP connection's peer address) , (peer's advertised address in peer_list)>.
    // This mapping is needed for proper cleanup on OnDisconnect.
    peer_addr_mapping: Arc<tokio::sync::RwLock<HashMap<SocketAddr, SocketAddr>>>,
    peer_list: Arc<tokio::sync::RwLock<BTreeSet<SocketAddr>>>,
    current_connecting_peer_list: Arc<tokio::sync::RwLock<BTreeSet<SocketAddr>>>,
    // Contains corrected peers that are used for asset file download.
    pub peer_http_port_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
    connect_lock: Arc<tokio::sync::Mutex<()>>,

    http_port: Option<u16>,
    nat_listen_addr: Option<SocketAddr>,
    psk: Vec<u8>,
    public_node_key: PublicKey,

    // Send Tx to the process loop.
    tx_sender: TxEventSender<P2pSender>,

    protocol_version: u64,
    node_resources: (u64, u64, u64),
}

impl Pea2Pea for P2P {
    fn node(&self) -> &Node {
        &self.node
    }
}

impl P2P {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        name: &str,
        listen_addr: SocketAddr,
        psk_passphrase: &str,
        public_node_key: PublicKey,
        http_port: Option<u16>,
        nat_listen_addr: Option<SocketAddr>,
        peer_http_port_list: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>>,
        tx_sender: TxEventSender<P2pSender>,
        propagate_tx_stream: impl Stream<Item=Transaction<Validated>> + std::marker::Send + 'static,
        node_resources: (u64, u64, u64),
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
            psk: psk.to_vec(),
            public_node_key,
            peer_list: Default::default(),
            current_connecting_peer_list: Default::default(),
            peer_addr_mapping: Default::default(),
            peer_http_port_list,
            connect_lock: Arc::new(tokio::sync::Mutex::new(())),
            http_port,
            nat_listen_addr,
            tx_sender,
            protocol_version: 1,
            node_resources,
        };

        // Enable node functionalities.
        instance.enable_handshake().await;
        instance.enable_reading().await;
        instance.enable_writing().await;
        instance.enable_disconnect().await;

        metrics::P2P_PROTOCOL_VERSION.set(instance.protocol_version as i64);

        // Start a new Tx stream loop.
        tokio::spawn({
            let p2p = instance.clone();
            async move {
                pin!(propagate_tx_stream);
                while let Some(tx) = propagate_tx_stream.next().await {
                    let tx_hash = tx.hash;
                    let msg = protocol::v0::Message::V0(protocol::v0::MessageV0::Transaction(tx));
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

    async fn forward_tx(&self, tx: Transaction<Created>) {
        tracing::debug!("submitting received tx to tx_handler");
        if let Err(err) = self.tx_sender.send_tx(tx) {
            tracing::error!("P2P error during received Tx sending:{err}");
        }
    }

    async fn build_handshake_msg(&self) -> protocol::internal::Handshake {
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

        protocol::internal::Handshake {
            my_p2p_listen_addr,
            peers,
            http_port: self.http_port,
        }
    }

    async fn process_diagnostics_request(
        &self,
        source: SocketAddr,
        req: protocol::internal::DiagnosticsRequestKind,
    ) -> io::Result<()> {
        let resp = match req {
            protocol::internal::DiagnosticsRequestKind::Version => {
                protocol::internal::Message::DiagnosticsResponse(
                    self.public_node_key,
                    protocol::internal::DiagnosticsResponse::Version {
                        major: env!("CARGO_PKG_VERSION_MAJOR").parse::<u16>().unwrap(),
                        minor: env!("CARGO_PKG_VERSION_MINOR").parse::<u16>().unwrap(),
                        patch: env!("CARGO_PKG_VERSION_PATCH").parse::<u16>().unwrap(),
                        build: format!(
                            "{}: {}",
                            env!("VERGEN_BUILD_TIMESTAMP"),
                            env!("VERGEN_GIT_DESCRIBE")
                        ),
                    },
                )
            }
            protocol::internal::DiagnosticsRequestKind::Resources => {
                let (cpus, mem, gpus) = self.node_resources;
                protocol::internal::Message::DiagnosticsResponse(
                    self.public_node_key,
                    protocol::internal::DiagnosticsResponse::Resources { cpus, mem, gpus },
                )
            }

            protocol::internal::DiagnosticsRequestKind::Metrics => {
                let data = tokio::task::spawn_blocking(metrics::export_metrics).await?;
                protocol::internal::Message::DiagnosticsResponse(
                    self.public_node_key,
                    protocol::internal::DiagnosticsResponse::Metrics(data),
                )
            }
        };

        let bs = Bytes::from(
            protocol::new_serialize_msg(self.protocol_version, resp)
                .expect("diagnostics response serialization"),
        );

        // Reply to requester.
        self.unicast(source, bs)?;

        Ok(())
    }

    // Connect to peer at `addr`. Subsequent connections to newly discovered nodes are done in sequence, one at a time.
    // Peer can be fail because they was 2 simultaneous connection. One is fail and the orher is ok.
    pub async fn connect(&self, addr: SocketAddr) -> (BTreeSet<SocketAddr>, BTreeSet<SocketAddr>) {
        self.do_connect(addr, false).await
    }
    pub async fn do_connect(&self, addr: SocketAddr, squelch_error: bool) -> (BTreeSet<SocketAddr>, BTreeSet<SocketAddr>) {
        let mut connected_peers = BTreeSet::new();
        let mut failed_peers = BTreeSet::new();
        let mut peer_to_connect_list = vec![addr];
        while !peer_to_connect_list.is_empty() {
            // Clear new peer list before connect
            self.current_connecting_peer_list.write().await.clear();
            let addr = peer_to_connect_list.pop().unwrap(); //unwrap tested in the while.
            match self.node.connect(addr).await {
                Ok(_) => {
                    connected_peers.insert(addr);
                    {
                        let peers = self.current_connecting_peer_list.write().await.clone();
                        peer_to_connect_list.extend(peers.iter());
                    };

                    // Only add peer that are connected.
                    self.peer_list.write().await.insert(addr);
                }
                Err(err) => {
                    if !squelch_error {
                        tracing::error!("An error occurs during peer:{addr} connection: {err}",);
                    }
                    failed_peers.insert(addr);
                }
            };
        }
        (connected_peers, failed_peers)
    }
}

#[async_trait::async_trait]
impl Handshake for P2P {
    async fn perform_handshake(&self, mut conn: Connection) -> io::Result<Connection> {
        // Sequentialize P2P connections to avoid deadlock.
        let _lock = self.connect_lock.lock().await;

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

        let mut peer_handshake_msg: protocol::internal::Handshake = match node_conn_side {
            ConnectionSide::Initiator => {
                // Send protocol version.
                stream.write_u64(self.protocol_version).await?;
                // Get Responder protocol version.
                let _protocol_version = stream.read_u64().await?;

                // Serialize & send our handshake message.
                let handshake_msg_bytes = protocol::serialize_handshake(
                    self.build_handshake_msg().await,
                )
                    .map_err(|err| {
                        std::io::Error::new(std::io::ErrorKind::Other, format!("serialize error:{err}"))
                    })?;
                stream.write_u32(handshake_msg_bytes.len() as u32).await?;
                stream.write_all(&handshake_msg_bytes).await?;

                // Receive handshake message from peer.
                let buffer_len = stream.read_u32().await? as usize;

                // TODO: Validate buffer length.
                let mut buffer = vec![0; buffer_len];
                stream.read_exact(&mut buffer).await?;

                protocol::deserialize_handshake(&buffer).map_err(|err| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("deserialize error:{err}"),
                    )
                })?
            }
            ConnectionSide::Responder => {
                // Get Initiator protocol version.
                let _protocol_version = stream.read_u64().await?;
                // Send protocol version.
                stream.write_u64(self.protocol_version).await?;

                // Receive the handshake message from the connecting peer.
                let buffer_len = stream.read_u32().await? as usize;
                let mut buffer = vec![0; buffer_len];
                stream.read_exact(&mut buffer).await?;

                let peer_handshake_msg: protocol::internal::Handshake =
                    protocol::deserialize_handshake(&buffer).map_err(|err| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("deserialize error:{err}"),
                        )
                    })?;

                // Serialize & send our handshake message.
                let handshake_msg_bytes = protocol::serialize_handshake(
                    self.build_handshake_msg().await,
                )
                    .map_err(|err| {
                        std::io::Error::new(std::io::ErrorKind::Other, format!("serialize error:{err}"))
                    })?;
                stream.write_u32(handshake_msg_bytes.len() as u32).await?;
                stream.write_all(&handshake_msg_bytes).await?;

                peer_handshake_msg
            }
        };

        // Current TCP connection peer address.
        let remote_peer = stream.peer_addr().unwrap();

        // Check if the remote P2P listen address needs to be updated from
        // the one observed in connection.
        let default_ip = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
        if peer_handshake_msg.my_p2p_listen_addr.ip() == default_ip {
            peer_handshake_msg
                .peers
                .remove(&peer_handshake_msg.my_p2p_listen_addr);
            peer_handshake_msg.my_p2p_listen_addr = SocketAddr::new(
                remote_peer.ip(),
                peer_handshake_msg.my_p2p_listen_addr.port(),
            );
            peer_handshake_msg
                .peers
                .insert(peer_handshake_msg.my_p2p_listen_addr);
        }

        tracing::debug!("tcp connection peer address: {}", remote_peer);
        tracing::debug!(
            "peer advertised address: {}",
            peer_handshake_msg.my_p2p_listen_addr
        );

        if tracing::enabled!(tracing::Level::DEBUG) {
            let print_peers: Vec<String> = peer_handshake_msg
                .peers
                .iter()
                .map(|x| x.to_string())
                .collect();
            tracing::debug!("peer contact list addresses: {:#?}", print_peers);
        }

        // Advertised remote peer listen address.
        let remote_peer_p2p_addr = &peer_handshake_msg.my_p2p_listen_addr;

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
            let local_diff: BTreeSet<SocketAddr> = peer_handshake_msg
                .peers
                .difference(&*local_peer_list)
                .cloned()
                .collect();

            // Add current connection peer.
            (*local_peer_list).insert(*remote_peer_p2p_addr);

            local_diff
        };

        local_diff.remove(&local_p2p_addr);
        local_diff.remove(remote_peer_p2p_addr);

        //add new peer to node list
        {
            self.current_connecting_peer_list
                .write()
                .await
                .append(&mut local_diff);
        }

        self.peer_http_port_list.write().await.insert(
            peer_handshake_msg.my_p2p_listen_addr,
            peer_handshake_msg.http_port,
        );

        metrics::P2P_CONNECTED_PEERS.inc();

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

        metrics::P2P_INCOMING_MESSAGES.inc();

        match protocol::deserialize_msg(message.as_ref()) {
            Ok(protocol::internal::Message::Transaction(tx)) => {
                tracing::debug!(
                    "received transaction {}:{} author:{}",
                    tx.hash,
                    tx.payload,
                    hex::encode(tx.author.serialize())
                );
                let tx: Transaction<Created> = Transaction {
                    author: tx.author,
                    hash: tx.hash,
                    payload: tx.payload,
                    nonce: tx.nonce,
                    signature: tx.signature,
                    propagated: tx.propagated,
                    executed: tx.executed,
                    state: Created,
                };
                self.forward_tx(tx).await;
            }
            Ok(protocol::internal::Message::DiagnosticsRequest(kind)) => {
                tracing::debug!("received diagnostics request");
                self.process_diagnostics_request(source, kind).await?;
            }
            // Nodes are expected to ignore the diagnostics response.
            Ok(protocol::internal::Message::DiagnosticsResponse(_, _)) => (),
            Err(err) => tracing::error!("failed to decode incoming transaction: {}", err),
        }

        Ok(())
    }
}

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
    use crate::txvalidation;
    use crate::txvalidation::CallbackSender;
    use crate::txvalidation::EventProcessError;
    use eyre::Result;
    use gevulot_node::types::transaction::Payload;
    use gevulot_node::types::transaction::Received;
    use libsecp256k1::SecretKey;
    use rand::{rngs::StdRng, SeedableRng};
    use tokio::sync::mpsc::UnboundedReceiver;
    use tokio::sync::mpsc::UnboundedSender;
    use tokio::sync::mpsc::{self};
    use tokio::sync::oneshot;
    use tokio_stream::wrappers::UnboundedReceiverStream;
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::EnvFilter;

    async fn create_peer(
        name: &str,
    ) -> (
        P2P,
        UnboundedSender<Transaction<Validated>>,
        UnboundedReceiver<(
            Transaction<Received>,
            Option<oneshot::Sender<Result<(), EventProcessError>>>,
        )>,
    ) {
        let http_peer_list1: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>> =
            Default::default();
        let (tx_sender, p2p_recv1) = mpsc::unbounded_channel::<Transaction<Validated>>();
        let p2p_stream1 = UnboundedReceiverStream::new(p2p_recv1);
        let (sendtx1, txreceiver1) =
            mpsc::unbounded_channel::<(Transaction<Received>, Option<CallbackSender>)>();
        let txsender1 = txvalidation::TxEventSender::<txvalidation::P2pSender>::build(sendtx1);
        let peer = P2P::new(
            name,
            "127.0.0.1:0".parse().unwrap(),
            "secret passphrase",
            PublicKey::from_secret_key(&SecretKey::default()),
            None,
            None,
            http_peer_list1,
            txsender1,
            p2p_stream1,
            (0, 0, 0),
        )
            .await;
        (peer, tx_sender, txreceiver1)
    }

    async fn create_faulty_peer(
        name: &str,
    ) -> (
        P2P,
        UnboundedSender<Transaction<Validated>>,
        UnboundedReceiver<(
            Transaction<Received>,
            Option<oneshot::Sender<Result<(), EventProcessError>>>,
        )>,
    ) {
        let http_peer_list1: Arc<tokio::sync::RwLock<HashMap<SocketAddr, Option<u16>>>> =
            Default::default();
        let (tx_sender, p2p_recv1) = mpsc::unbounded_channel::<Transaction<Validated>>();
        let p2p_stream1 = UnboundedReceiverStream::new(p2p_recv1);
        let (sendtx1, txreceiver1) =
            mpsc::unbounded_channel::<(Transaction<Received>, Option<CallbackSender>)>();
        let txsender1 = txvalidation::TxEventSender::<txvalidation::P2pSender>::build(sendtx1);
        let peer = P2P::new(
            name,
            "127.0.0.1:0".parse().unwrap(),
            "secret passphrase",
            PublicKey::from_secret_key(&SecretKey::default()),
            None,
            //set a bad public addr so that other peer can't connect.
            Some("128.0.0.1:0".parse().unwrap()),
            http_peer_list1,
            txsender1,
            p2p_stream1,
            (0, 0, 0),
        )
            .await;
        (peer, tx_sender, txreceiver1)
    }

    // TODO: Change to `impl From` form when module declaration between main and lib is solved.
    fn into_receive(tx: Transaction<Validated>) -> Transaction<Received> {
        Transaction {
            author: tx.author,
            hash: tx.hash,
            payload: tx.payload,
            nonce: tx.nonce,
            signature: tx.signature,
            propagated: tx.executed,
            executed: tx.executed,
            state: Received::P2P,
        }
    }

    // Faulty Peer2 -> connect to Peer1
    // Peer3 -> connect to Peer1
    // Automaique Peer3 -> Peer2 connection fail
    // but Peer1 and Peer2 are connected
    #[tokio::test]
    async fn test_one_peer_fail() {
        //start_logger(LevelFilter::ERROR);

        let (peer1, tx_sender1, mut tx_receiver1) = create_peer("peer1").await;
        let (peer2, tx_sender2, mut tx_receiver2) = create_faulty_peer("peer2").await;
        let (peer3, tx_sender3, mut tx_receiver3) = create_peer("peer3").await;

        peer1.node().start_listening().await.expect("peer1 listen");
        peer2.node().start_listening().await.expect("peer2 listen");
        peer3.node().start_listening().await.expect("peer3 listen");

        let (new_peers, fail_peers) = peer2.connect(peer1.node().listening_addr().unwrap()).await;

        assert_eq!(new_peers.len(), 1);
        assert_eq!(fail_peers.len(), 0);

        let (new_peers, fail_peers) = peer3.connect(peer1.node().listening_addr().unwrap()).await;

        assert_eq!(new_peers.len(), 1);
        assert_eq!(fail_peers.len(), 1);
        assert_eq!(fail_peers.first(), Some(&"128.0.0.1:0".parse().unwrap()));

        // Wait for the connection fail timeout.
        tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;

        assert_eq!(peer1.peer_http_port_list.read().await.len(), 2);
        assert_eq!(peer2.peer_http_port_list.read().await.len(), 1);
        assert_eq!(peer3.peer_http_port_list.read().await.len(), 1);

        //  Peer2 -> Peer1 works
        let tx = new_tx();
        tx_sender2.send(tx.clone()).unwrap();
        let recv_tx = tx_receiver1.recv().await.expect("peer1 recv");
        assert_eq!(into_receive(tx.clone()), recv_tx.0);

        // Peer2 not connectred to Peer3
        match tx_receiver3.try_recv() {
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => (),
            _ => panic!("Peer2 connected and it shouldn't"),
        }

        // Peer3 -> Peer1 works
        let tx = new_tx();
        tx_sender3.send(tx.clone()).unwrap();
        let recv_tx = tx_receiver1.recv().await.expect("peer1 recv");
        assert_eq!(into_receive(tx.clone()), recv_tx.0);

        //  Peer2 not connected to Peer3
        match tx_receiver2.try_recv() {
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => (),
            _ => panic!("Peer2 connected and it shouldn't"),
        }
    }

    // 5 peers
    // Peer2 connect to Peer1.
    // Peer3 connect to Peer1.
    // Peer3 automatically connect to Peer2.
    // Peer4 connect to Peer3.
    // Peer4 automatically connect to Peer1, Peer2.
    // Peer5 connect to Peer4.
    // Peer5 automatically connect to Peer1, Peer2, Peer3.
    #[tokio::test]
    async fn test_peer_list_inter_connection() {
        //start_logger(LevelFilter::ERROR);

        let (peer1, tx_sender1, mut tx_receiver1) = create_peer("peer1").await;
        let (peer2, tx_sender2, mut tx_receiver2) = create_peer("peer2").await;
        let (peer3, tx_sender3, mut tx_receiver3) = create_peer("peer3").await;
        let (peer4, tx_sender4, mut tx_receiver4) = create_peer("peer4").await;
        let (peer5, tx_sender5, mut tx_receiver5) = create_peer("peer5").await;

        let bind_add = peer1.node().start_listening().await.expect("peer1 listen");
        let bind_add = peer2.node().start_listening().await.expect("peer2 listen");
        let bind_add = peer3.node().start_listening().await.expect("peer3 listen");
        let bind_add = peer4.node().start_listening().await.expect("peer4 listen");
        let bind_add = peer5.node().start_listening().await.expect("peer5 listen");

        let (new_peers, fail_peers) = peer2.connect(peer1.node().listening_addr().unwrap()).await;
        assert_eq!(new_peers.len(), 1);

        assert_eq!(peer1.peer_http_port_list.read().await.len(), 1);
        assert_eq!(peer2.peer_http_port_list.read().await.len(), 1);

        let (new_peers, fail_peers) = peer3.connect(peer1.node().listening_addr().unwrap()).await;
        assert_eq!(new_peers.len(), 2);

        let (new_peers, fail_peers) = peer4.connect(peer3.node().listening_addr().unwrap()).await;
        assert_eq!(new_peers.len(), 3);

        let (new_peers, fail_peers) = peer5.connect(peer4.node().listening_addr().unwrap()).await;
        assert_eq!(new_peers.len(), 4);

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        assert_eq!(peer1.peer_http_port_list.read().await.len(), 4);
        assert_eq!(peer2.peer_http_port_list.read().await.len(), 4);
        assert_eq!(peer3.peer_http_port_list.read().await.len(), 4);
        assert_eq!(peer4.peer_http_port_list.read().await.len(), 4);
        assert_eq!(peer5.peer_http_port_list.read().await.len(), 4);

        // Verify connections by sending Tx to all peers.
        let tx = new_tx();
        tx_sender2.send(tx.clone()).unwrap();
        let recv_tx = tx_receiver1.recv().await.expect("peer1 recv");
        assert_eq!(into_receive(tx.clone()), recv_tx.0);

        let recv_tx = tx_receiver3.recv().await.expect("peer3 recv");
        assert_eq!(into_receive(tx.clone()), recv_tx.0);

        let recv_tx = tx_receiver4.recv().await.expect("peer4 recv");
        assert_eq!(into_receive(tx.clone()), recv_tx.0);

        let recv_tx = tx_receiver5.recv().await.expect("peer5 recv");
        assert_eq!(into_receive(tx.clone()), recv_tx.0);

        let tx = new_tx();
        tx_sender5.send(tx.clone()).unwrap();

        let recv_tx = tx_receiver1.recv().await.expect("peer1 recv");
        assert_eq!(into_receive(tx.clone()), recv_tx.0);

        let recv_tx = tx_receiver2.recv().await.expect("peer2 recv");
        assert_eq!(into_receive(tx.clone()), recv_tx.0);

        let recv_tx = tx_receiver3.recv().await.expect("peer3 recv");
        assert_eq!(into_receive(tx.clone()), recv_tx.0);

        let recv_tx = tx_receiver4.recv().await.expect("peer4 recv");
        assert_eq!(into_receive(tx), recv_tx.0);
    }

    // Test  Peer2 that  disconnect.
    // It should be removed from Peer1 peers list.
    #[tokio::test]
    async fn test_two_peers_disconnect() {
        //start_logger(LevelFilter::ERROR);

        let (peer1, tx_sender1, mut tx_receiver1) = create_peer("peer1").await;
        peer1.node().start_listening().await.expect("peer1 listen");

        {
            let (peer2, tx_sender2, mut tx_receiver2) = create_peer("peer2").await;
            peer2.node().start_listening().await.expect("peer2 listen");

            let (new_peers, fail_peers) =
                peer1.connect(peer2.node().listening_addr().unwrap()).await;
            assert_eq!(new_peers.len(), 1);
            assert_eq!(fail_peers.len(), 0);
            assert_eq!(peer1.peer_http_port_list.read().await.len(), 1);
            assert_eq!(peer2.peer_http_port_list.read().await.len(), 1);

            let tx = new_tx();
            tx_sender1.send(tx.clone()).unwrap();
            let recv_tx = tx_receiver2.recv().await.expect("peer2 recv");
            assert_eq!(into_receive(tx), recv_tx.0);

            let tx = new_tx();
            tx_sender2.send(tx.clone()).unwrap();
            let recv_tx = tx_receiver1.recv().await.expect("peer1 recv");
            assert_eq!(into_receive(tx), recv_tx.0);

            // Force manual disconnect because dropping the node don't disconnect peers.
            let peers = peer2.node().connected_addrs();
            for addr in peers {
                peer2.node().disconnect(addr).await;
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        let tx = new_tx();
        tx_sender1.send(tx).unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        assert_eq!(peer1.peer_list.read().await.len(), 1);
        assert!(peer1.peer_addr_mapping.read().await.is_empty());
        assert_eq!(peer1.peer_http_port_list.read().await.len(), 0);
        assert_eq!(peer1.peer_http_port_list.read().await.len(), 0);
    }

    // Test 2 peers that connect each other.
    #[tokio::test]
    async fn test_two_peers() {
        //start_logger(LevelFilter::ERROR);

        let (peer1, tx_sender1, mut tx_receiver1) = create_peer("peer1").await;
        let (peer2, tx_sender2, mut tx_receiver2) = create_peer("peer2").await;

        peer1.node().start_listening().await.expect("peer1 listen");
        peer2.node().start_listening().await.expect("peer2 listen");

        let (new_peers, fail_peers) = peer2.connect(peer1.node().listening_addr().unwrap()).await;
        assert_eq!(new_peers.len(), 1);
        assert_eq!(fail_peers.len(), 0);

        let tx = new_tx();
        tx_sender1.send(tx.clone()).unwrap();
        let recv_tx = tx_receiver2.recv().await.expect("peer2 recv");
        assert_eq!(into_receive(tx), recv_tx.0);

        let tx = new_tx();
        tx_sender2.send(tx.clone()).unwrap();
        let recv_tx = tx_receiver1.recv().await.expect("peer1 recv");
        assert_eq!(into_receive(tx), recv_tx.0);
    }

    fn new_tx() -> Transaction<Validated> {
        let rng = &mut StdRng::from_entropy();

        let tx = Transaction::<Created>::new(Payload::Empty, &SecretKey::random(rng));

        Transaction {
            author: tx.author,
            hash: tx.hash,
            payload: tx.payload,
            nonce: tx.nonce,
            signature: tx.signature,
            propagated: tx.executed,
            executed: tx.executed,
            state: Validated,
        }
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
