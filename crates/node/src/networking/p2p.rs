use std::{
    collections::{BTreeSet, HashMap},
    io,
    net::SocketAddr,
    str,
    sync::Arc,
};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

use super::noise;
use bytes::{Bytes, BytesMut};
use eyre::Result;
use gevulot_node::types::Transaction;
use parking_lot::RwLock;
use pea2pea::{
    protocols::{Handshake, OnDisconnect, Reading, Writing},
    Config, Connection, ConnectionSide, Node, Pea2Pea,
};
use sha3::{Digest, Sha3_256};

#[async_trait::async_trait]
pub trait TxHandler: Send + Sync {
    async fn recv_tx(&self, tx: Transaction) -> Result<()>;
}

#[async_trait::async_trait]
pub trait TxChannel: Send + Sync {
    async fn send_tx(&self, tx: &Transaction) -> Result<()>;
}

struct BlackholeTxHandler;
#[async_trait::async_trait]
impl TxHandler for BlackholeTxHandler {
    async fn recv_tx(&self, tx: Transaction) -> Result<()> {
        tracing::debug!("submitting received tx to black hole");
        Ok(())
    }
}

// NOTE: This P2P implementation is originally from `pea2pea` Noise handshake example.
#[derive(Clone)]
pub struct P2P {
    node: Node,
    noise_states: Arc<RwLock<HashMap<SocketAddr, noise::State>>>,
    tx_handler: Arc<tokio::sync::RwLock<Arc<dyn TxHandler>>>,
    psk: Vec<u8>,
    peer_list: Arc<RwLock<BTreeSet<SocketAddr>>>,
    //Map to connection local addr notified on_disconnect and the peer connection addr (peer_list).
    peer_addr_mapping: Arc<RwLock<HashMap<SocketAddr, SocketAddr>>>,
}

impl Pea2Pea for P2P {
    fn node(&self) -> &Node {
        &self.node
    }
}

impl P2P {
    pub async fn new(name: &str, listen_addr: SocketAddr, psk_passphrase: &str) -> Self {
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
            tx_handler: Arc::new(tokio::sync::RwLock::new(Arc::new(BlackholeTxHandler {}))),
            psk: psk.to_vec(),
            peer_list: Default::default(),
            peer_addr_mapping: Default::default(),
        };

        // Enable node functionalities.
        instance.enable_handshake().await;
        instance.enable_reading().await;
        instance.enable_writing().await;
        instance.enable_disconnect().await;

        instance
    }

    pub async fn register_tx_handler(&self, tx_handler: Arc<dyn TxHandler>) {
        let mut old_handler = self.tx_handler.write().await;
        *old_handler = tx_handler;
        tracing::debug!("new tx handler registered");
    }

    async fn recv_tx(&self, tx: Transaction) {
        tracing::debug!("submitting received tx to tx_handler");
        let tx_handler = self.tx_handler.read().await;
        if let Err(err) = tx_handler.recv_tx(tx).await {
            tracing::error!("failed to handle incoming transaction: {}", err);
        } else {
            tracing::debug!("submitted received tx to tx_handler");
        }
    }
}

#[async_trait::async_trait]
impl Handshake for P2P {
    async fn perform_handshake(&self, mut conn: Connection) -> io::Result<Connection> {
        // create the noise objects
        let noise_builder =
            snow::Builder::new("Noise_XXpsk3_25519_ChaChaPoly_BLAKE2s".parse().unwrap());
        let noise_keypair = noise_builder.generate_keypair().unwrap();
        let noise_builder = noise_builder.local_private_key(&noise_keypair.private);
        let noise_builder = noise_builder.psk(3, self.psk.as_slice());

        // perform the noise handshake
        let (noise_state, _) =
            noise::handshake_xx(self, &mut conn, noise_builder, Bytes::new()).await?;

        // save the noise state to be reused by Reading and Writing
        self.noise_states.write().insert(conn.addr(), noise_state);

        //exchange peer list

        let node_conn_side = !conn.side();
        let stream = self.borrow_stream(&mut conn);

        let local_bind_addr = self.node.listening_addr().unwrap();
        let peer_list_bytes: Vec<u8> = {
            let peer_list: &mut BTreeSet<SocketAddr> = &mut self.peer_list.write();
            peer_list.insert(local_bind_addr); //add it if not present.
            bincode::serialize(peer_list).map_err(|err| {
                std::io::Error::new(std::io::ErrorKind::Other, format!("serialize error:{err}"))
            })?
        };

        let (distant_peer_list, distant_listening_addr) = match node_conn_side {
            ConnectionSide::Initiator => {
                //on_disconnect doesn't notify with the bind port but the connect port.
                //can't be use to open a connection.
                //So the connecting node notify it's bind address.
                let bind_addr_bytes = bincode::serialize(&self.node.listening_addr().unwrap())
                    .map_err(|err| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("serialize error:{err}"),
                        )
                    })?;
                stream.write_u32(bind_addr_bytes.len() as u32).await?;
                stream.write_all(&bind_addr_bytes).await?;

                //send peer list
                stream.write_u32(peer_list_bytes.len() as u32).await?;
                stream.write_all(&peer_list_bytes).await?;

                // receive the peer list
                let buffer_len = stream.read_u32().await? as usize;
                //TODO validate buffer lengh
                let mut buffer = vec![0; buffer_len];
                stream.read_exact(&mut buffer).await?;
                let distant_peer_list: BTreeSet<SocketAddr> = bincode::deserialize(&buffer)
                    .map_err(|err| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("deserialize error:{err}"),
                        )
                    })?;

                self.peer_addr_mapping
                    .write()
                    .insert(stream.peer_addr().unwrap(), stream.peer_addr().unwrap());

                (distant_peer_list, stream.peer_addr().unwrap())
            }
            ConnectionSide::Responder => {
                //receive the connecting node addr
                let buffer_len = stream.read_u32().await? as usize;
                let mut buffer = vec![0; buffer_len];
                stream.read_exact(&mut buffer).await?;
                let distant_listening_addr: SocketAddr =
                    bincode::deserialize(&buffer).map_err(|err| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("deserialize error:{err}"),
                        )
                    })?;
                {
                    self.peer_list.write().insert(distant_listening_addr);
                    self.peer_addr_mapping
                        .write()
                        .insert(stream.peer_addr().unwrap(), distant_listening_addr);
                }

                // receive the peer list
                let buffer_len = stream.read_u32().await? as usize;
                let mut buffer = vec![0; buffer_len];
                stream.read_exact(&mut buffer).await?;
                let distant_peer_list: BTreeSet<SocketAddr> = bincode::deserialize(&buffer)
                    .map_err(|err| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            format!("deserialize error:{err}"),
                        )
                    })?;

                //send peer list
                stream.write_u32(peer_list_bytes.len() as u32).await?;
                stream.write_all(&peer_list_bytes).await?;

                (distant_peer_list, distant_listening_addr)
            }
        };

        //do peer comparition
        let mut local_diff = {
            let local_peer_list: &mut BTreeSet<SocketAddr> = &mut self.peer_list.write();
            let distant_diff: BTreeSet<SocketAddr> = local_peer_list
                .difference(&distant_peer_list)
                .cloned()
                .collect();
            let local_diff: BTreeSet<SocketAddr> = distant_peer_list
                .difference(&local_peer_list)
                .cloned()
                .collect();

            local_peer_list.append(&mut local_diff.iter().cloned().collect());
            local_diff
        };
        local_diff.remove(&local_bind_addr);
        local_diff.remove(&distant_listening_addr);

        let node = self.node();
        for addr in local_diff {
            //the return error is not use because:
            //already logged and mostly because there's double connection between 2 peers.
            let _ = node.connect(addr).await;
        }

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
            Ok(tx) => self.recv_tx(tx).await,
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
        if let Some(peer_conn_addr) = self.peer_addr_mapping.write().remove(&addr) {
            self.peer_list.write().remove(&peer_conn_addr);
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
        let (peer1, peer2, peer3) = (
            P2P::new("peer1", "127.0.0.1:0".parse().unwrap(), "secret passphrase").await,
            P2P::new("peer2", "127.0.0.1:0".parse().unwrap(), "secret passphrase").await,
            P2P::new("peer3", "127.0.0.1:0".parse().unwrap(), "secret passphrase").await,
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

        tracing::debug!("connect peer3 to peer1");
        peer3
            .node()
            .connect(peer1.node().listening_addr().unwrap())
            .await
            .unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        //assert_eq!(peer1.get_connected_peers().len(), 3);
        //assert_eq!(peer2.get_connected_peers().len(), 3);
        //assert_eq!(peer3.get_connected_peers().len(), 3);

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

        let peer1 = P2P::new("peer1", "127.0.0.1:0".parse().unwrap(), "secret passphrase").await;
        peer1.node().start_listening().await.expect("peer1 listen");
        peer1.register_tx_handler(sink1.clone()).await;

        {
            let peer2 =
                P2P::new("peer2", "127.0.0.1:0".parse().unwrap(), "secret passphrase").await;
            peer2.node().start_listening().await.expect("peer2 listen");

            peer2.register_tx_handler(sink2.clone()).await;
            tracing::debug!("Nodes init Done");

            peer1
                .node()
                .connect(peer2.node().listening_addr().unwrap())
                .await
                .unwrap();

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

        //simulate the silent node de-connection by dropping the node.
        tracing::debug!("send tx from peer1 to disconnected peer2");
        let tx = new_tx();
        peer1.send_tx(&tx).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        assert_eq!((&peer1.peer_list.read()).len(), 1);
        assert!((&peer1.peer_addr_mapping.read()).is_empty());
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
            P2P::new("peer1", "127.0.0.1:0".parse().unwrap(), "secret passphrase").await,
            P2P::new("peer2", "127.0.0.1:0".parse().unwrap(), "secret passphrase").await,
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
