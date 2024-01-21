use std::{collections::HashMap, io, net::SocketAddr, str, sync::Arc};

use bytes::{Bytes, BytesMut};
use eyre::Result;
use gevulot_node::types::Transaction;
use parking_lot::RwLock;
use pea2pea::{
    protocols::{Handshake, Reading, Writing},
    Config, Connection, ConnectionSide, Node, Pea2Pea,
};
use sha3::{Digest, Sha3_256};

use super::noise;

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
        };

        // Enable node functionalities.
        instance.enable_handshake().await;
        instance.enable_reading().await;
        instance.enable_writing().await;

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

#[cfg(test)]
mod tests {
    use super::*;
    use eyre::Result;
    use gevulot_node::types::{transaction::Payload, Transaction};
    use libsecp256k1::SecretKey;
    use rand::{rngs::StdRng, SeedableRng};
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
    async fn test_two_peers() {
        start_logger(LevelFilter::ERROR);

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
        let key = SecretKey::random(rng);
        Transaction::new(Payload::Empty, &key)
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
