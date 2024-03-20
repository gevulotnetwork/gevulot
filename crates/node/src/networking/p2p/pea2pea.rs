use super::protocol;
use crate::txvalidation::P2pSender;
use crate::txvalidation::TxEventSender;
use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use gevulot_node::types::{
    transaction::{Created, Validated},
    Transaction,
};
use libsecp256k1::PublicKey;
use pea2pea::{
    protocols::{Reading, Writing},
    Config, ConnectionSide, Node, Pea2Pea,
};
use sha3::{Digest, Sha3_256};
use std::{
    collections::{BTreeSet, HashMap},
    io,
    net::SocketAddr,
    str,
    sync::Arc,
};
use tokio_util::codec::Decoder;
use tokio_util::codec::Encoder;

// NOTE: This P2P implementation is originally from `pea2pea` Noise handshake example.
#[derive(Clone)]
pub struct P2P {
    node: Node,
    pub peer_list: Arc<BTreeSet<SocketAddr>>,
    // Contains corrected peers that are used for asset file download.
    http_port: Option<u16>,
    nat_listen_addr: Option<SocketAddr>,
    psk: Vec<u8>,
    public_node_key: PublicKey,
    // Send Tx to the process loop.
    tx_sender: TxEventSender<P2pSender>,
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
        propagate_tx_stream: impl Stream<Item = Transaction<Validated>> + std::marker::Send + 'static,
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
            psk: psk.to_vec(),
            public_node_key,
            peer_list: Default::default(),
            http_port,
            nat_listen_addr,
            tx_sender,
        };

        // Enable node functionalities.
        //instance.enable_handshake().await;
        instance.enable_reading().await;
        instance.enable_writing().await;
        //instance.enable_disconnect().await;

        instance
    }

    async fn forward_tx(&self, tx: Transaction<Created>) {
        tracing::debug!("submitting received tx to tx_handler");
        if let Err(err) = self.tx_sender.send_tx(tx) {
            tracing::error!("P2P error during received Tx sending:{err}");
        }
    }
}

pub struct TestCodec(tokio_util::codec::BytesCodec);

impl Default for TestCodec {
    fn default() -> Self {
        let inner = tokio_util::codec::BytesCodec::new();
        Self(inner)
    }
}

impl Decoder for TestCodec {
    type Item = BytesMut;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.0.decode(src)
    }
}

impl Encoder<Bytes> for TestCodec {
    type Error = io::Error;

    fn encode(&mut self, item: Bytes, dst: &mut BytesMut) -> Result<(), Self::Error> {
        self.0.encode(item, dst)
    }
}

#[async_trait::async_trait]
impl Reading for P2P {
    type Message = BytesMut;
    type Codec = TestCodec;

    fn codec(&self, addr: SocketAddr, _side: ConnectionSide) -> Self::Codec {
        Default::default()
    }

    async fn process_message(&self, source: SocketAddr, message: Self::Message) -> io::Result<()> {
        tracing::debug!(parent: self.node().span(), "decrypted a message from {}", source);

        match bincode::deserialize(message.as_ref()) {
            Ok(protocol::Message::V0(msg)) => match msg {
                protocol::MessageV0::Transaction(tx) => {
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
                protocol::MessageV0::DiagnosticsRequest(kind) => (),
                // Nodes are expected to ignore the diagnostics response.
                protocol::MessageV0::DiagnosticsResponse(_, _) => (),
            },
            Err(err) => tracing::error!("failed to decode incoming transaction: {}", err),
        }

        Ok(())
    }
}

impl Writing for P2P {
    type Message = Bytes;
    type Codec = TestCodec;

    fn codec(&self, addr: SocketAddr, _side: ConnectionSide) -> Self::Codec {
        Default::default()
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
    use rand::Rng;
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

    // Test 20 peers that connect each other.
    #[tokio::test]
    async fn test_twenty_peers() {
        start_logger(LevelFilter::TRACE);

        struct Peer {
            p2p: P2P,
            tx_sender: UnboundedSender<Transaction<Validated>>,
            tx_receiver: UnboundedReceiver<(
                Transaction<Received>,
                Option<oneshot::Sender<Result<(), EventProcessError>>>,
            )>,
        }

        impl Peer {
            fn new(
                tuple: (
                    P2P,
                    UnboundedSender<Transaction<Validated>>,
                    UnboundedReceiver<(
                        Transaction<Received>,
                        Option<oneshot::Sender<Result<(), EventProcessError>>>,
                    )>,
                ),
            ) -> Peer {
                Peer {
                    p2p: tuple.0,
                    tx_sender: tuple.1,
                    tx_receiver: tuple.2,
                }
            }

            fn send_tx(&self, tx: Transaction<Validated>) -> Result<()> {
                let tx_hash = tx.hash;
                let msg = protocol::Message::V0(protocol::MessageV0::Transaction(tx));
                let bs = bincode::serialize(&msg)?;
                let bs = Bytes::from(bs);
                tracing::debug!("broadcasting transaction {}", tx_hash);
                self.p2p.broadcast(bs)?;
                Ok(())
            }
        }

        tracing::debug!("creating P2P beacon node");
        let mut p2p_beacon_node = Peer::new(create_peer(&format!("p2p-beacon")).await);

        let num_nodes = 20;
        let mut peers = Vec::with_capacity(num_nodes);
        let mut peer_list: BTreeSet<SocketAddr> = BTreeSet::new();

        //first connect
        for i in 1..num_nodes {
            tracing::debug!("creating peer {i}");
            let peer = Peer::new(create_peer(&format!("peer{i}")).await);

            tracing::debug!("peer{i} starts listening");
            let addr = peer
                .p2p
                .node()
                .start_listening()
                .await
                .expect("peer{i] listen");
            tracing::debug!("peer{i} is listening");
            peers.push((peer, addr));
            peer_list.insert(addr);
        }

        p2p_beacon_node.p2p.peer_list = Arc::new(peer_list);

        tracing::debug!("P2P beacon node starts listening");
        p2p_beacon_node
            .p2p
            .node()
            .start_listening()
            .await
            .expect("p2p_beacon_node start_listening()");
        tracing::debug!("P2P beacon node is listening");

        let mut connected_nodes: BTreeSet<SocketAddr> = BTreeSet::new();
        for (i, (peer, addr)) in peers.iter().enumerate() {
            tracing::debug!("peer{i} connects to P2P beacon node");
            peer.p2p
                .node()
                .connect(
                    p2p_beacon_node
                        .p2p
                        .node()
                        .listening_addr()
                        .expect("p2p_beacon_node listening_addr()"),
                )
                .await
                .expect(&format!("peer{i} connect p2p_beacon_node"));
            tracing::debug!("peer{i} is connected to P2P beacon node");
            //connect to other connected peers
            for addr in &connected_nodes {
                tokio::spawn({
                    let node = peer.p2p.node.clone();
                    let addr = *addr;
                    //let peer_list = self.peer_list.clone();
                    async move {
                        tracing::debug!("connect to {}", &addr);

                        // XXX: If `node.connect(addr)` returns an error, it's omitted because:
                        // 1.) It's already logged.
                        // 2.) It often happens because there is already a connection between the 2 peers.
                        match node.connect(addr).await {
                            Ok(_) => {
                                //peer_list.write().await.insert(addr);
                                tracing::debug!("connected to {}", &addr);
                            }
                            Err(err) => tracing::error!("failed to connect to {}: {}", &addr, err),
                        };
                    }
                });
            }
            connected_nodes.insert(*addr);
        }

        tracing::info!("All {num_nodes} P2P nodes started");

        // Let the dust settle down a bit.
        //tokio::time::sleep(std::time::Duration::from_secs(5)).await;

        // Broadcast 50 transactions into the network.
        for i in 1..50 {
            // Create transaction.
            let tx = new_tx();

            // Pick random node to submit it.
            let node_idx = rand::thread_rng().gen_range(0..peers.len());

            tracing::debug!("sending transaction {i} from peer {node_idx}");
            peers[node_idx]
                .0
                .send_tx(tx.clone())
                .expect(&format!("peer{node_idx} send()"));
            tracing::debug!("transaction {i} sent from peer {node_idx}");

            for i in 1..peers.len() {
                if i == node_idx {
                    continue;
                }

                tracing::debug!("receiving transaction {i} on peer {i}");
                let recv_tx = peers[i]
                    .0
                    .tx_receiver
                    .recv()
                    .await
                    .expect(&format!("peers[{i}] recv()"));
                tracing::debug!("received transaction {i} on peer {i}");
                assert_eq!(into_receive(tx.clone()), recv_tx.0);
            }

            //tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
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
