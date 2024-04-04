use eyre::Result;
use prometheus_hyper::Server;
use std::{net::SocketAddr, sync::Arc};

use lazy_static::lazy_static;
use prometheus::{HistogramOpts, HistogramVec, IntCounter, IntGauge, Registry};

lazy_static! {
    pub static ref REGISTRY: Arc<Registry> = Arc::new(Registry::new());

    // RPC metrics.
    pub static ref RPC_INCOMING_REQUESTS: IntCounter =
        IntCounter::new("rpc_incoming_requests", "Incoming RPC Requests")
            .expect("metric can be created");
    pub static ref RPC_RESPONSE_TIME_COLLECTOR: HistogramVec = HistogramVec::new(
        HistogramOpts::new("rpc_response_time", "RPC Response Times"),
        &["method"]
    )
    .expect("metric can be created");

    // P2P metrics.
    pub static ref P2P_PROTOCOL_VERSION: IntGauge =
        IntGauge::new("p2p_protocol_version", "P2P Protocol Version").expect("metric can be created");
    pub static ref P2P_CONNECTED_PEERS: IntGauge =
        IntGauge::new("p2p_connected_peers", "Connected P2P Peers").expect("metric can be created");
    pub static ref P2P_INCOMING_MESSAGES: IntCounter =
        IntCounter::new("p2p_incoming_messages", "Incoming P2P Messages")
            .expect("metric can be created");


    // Transaction metrics.
    pub static ref TX_EXECUTION_TIME_COLLECTOR: HistogramVec = HistogramVec::new(
        HistogramOpts::new("tx_execution_time", "Transaction Execution Times (ms)"),
        &["kind","status"]
    )
    .expect("metric can be created");
    pub static ref TX_SCHEDULING_REQUEUED: IntCounter =
        IntCounter::new("tx_scheduling_requeued", "Transaction Requeued in Scheduling")
            .expect("metric can be created");

    // Resources metrics.
    pub static ref CPUS_AVAILABLE: IntGauge =
        IntGauge::new("gevulot_cpus_available", "Available CPUs in Gevulot")
            .expect("metric can be created");
    pub static ref MEM_AVAILABLE: IntGauge =
        IntGauge::new("gevulot_mem_available", "Available MEM in Gevulot")
            .expect("metric can be created");
    pub static ref GPUS_AVAILABLE: IntGauge =
        IntGauge::new("gevulot_gpus_available", "Available GPUs in Gevulot")
            .expect("metric can be created");
    pub static ref CPUS_TOTAL: IntGauge =
        IntGauge::new("gevulot_cpus_total", "Total number of CPUs in Gevulot")
            .expect("metric can be created");
    pub static ref MEM_TOTAL: IntGauge =
        IntGauge::new("gevulot_mem_total", "Total amount of MEM in Gevulot")
            .expect("metric can be created");
    pub static ref GPUS_TOTAL: IntGauge =
        IntGauge::new("gevulot_gpus_total", "Total number of GPUs in Gevulot")
            .expect("metric can be created");
}

pub(crate) fn register_metrics() {
    REGISTRY
        .register(Box::new(RPC_INCOMING_REQUESTS.clone()))
        .expect("collector can be registered");

    REGISTRY
        .register(Box::new(RPC_RESPONSE_TIME_COLLECTOR.clone()))
        .expect("collector can be registered");

    REGISTRY
        .register(Box::new(P2P_PROTOCOL_VERSION.clone()))
        .expect("collector can be registered");
    REGISTRY
        .register(Box::new(P2P_CONNECTED_PEERS.clone()))
        .expect("collector can be registered");
    REGISTRY
        .register(Box::new(P2P_INCOMING_MESSAGES.clone()))
        .expect("collector can be registered");

    REGISTRY
        .register(Box::new(TX_EXECUTION_TIME_COLLECTOR.clone()))
        .expect("collector can be registered");
    REGISTRY
        .register(Box::new(TX_SCHEDULING_REQUEUED.clone()))
        .expect("collector can be registered");

    REGISTRY
        .register(Box::new(CPUS_AVAILABLE.clone()))
        .expect("collector can be registered");
    REGISTRY
        .register(Box::new(MEM_AVAILABLE.clone()))
        .expect("collector can be registered");
    REGISTRY
        .register(Box::new(GPUS_AVAILABLE.clone()))
        .expect("collector can be registered");
    REGISTRY
        .register(Box::new(CPUS_TOTAL.clone()))
        .expect("collector can be registered");
    REGISTRY
        .register(Box::new(MEM_TOTAL.clone()))
        .expect("collector can be registered");
    REGISTRY
        .register(Box::new(GPUS_TOTAL.clone()))
        .expect("collector can be registered");
}

pub(crate) async fn serve_metrics(bind_addr: SocketAddr) -> Result<()> {
    // Start Server endlessly.
    tokio::spawn(async move {
        Server::run(REGISTRY.clone(), bind_addr, futures_util::future::pending()).await
    });

    Ok(())
}
