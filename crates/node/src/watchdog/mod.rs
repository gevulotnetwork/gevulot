use eyre::Result;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::select;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio::time::Duration;

pub const GRACEFULL_SIGNAL_COUNTER_LIMIT: usize = 20;
pub const SCHEDULER_HEALTH_SIGNAL_TIMEOUT_MILLIS: Duration = Duration::from_millis(1000);
pub const NO_LOOP_DETECT_TIMEOUT_MILLIS: Duration = Duration::from_millis(1000);

#[derive(Clone, Debug, Copy, PartialEq, PartialOrd)]
pub enum HealthCheckSignal {
    SchedulerLoopOk,
    SchedulerMempoolLen(usize),
}

#[derive(Clone, Debug, Copy, PartialEq, PartialOrd)]
enum WatchDogState {
    Alive,
    Graceful,
    Critical,
}

impl From<WatchDogState> for StatusCode {
    fn from(state: WatchDogState) -> Self {
        match state {
            WatchDogState::Alive => StatusCode::OK,
            WatchDogState::Graceful => StatusCode::NO_CONTENT,
            WatchDogState::Critical => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

pub async fn start_watchdog(bind_addr: SocketAddr) -> Result<mpsc::Sender<HealthCheckSignal>> {
    let (scheduler_health_tx, scheduler_health_rx) = mpsc::channel::<HealthCheckSignal>(100);
    let watchdog_state = Arc::new(Mutex::new(WatchDogState::Alive));
    tokio::spawn({
        let watchdog_state = watchdog_state.clone();
        async move {
            run_watchdog(
                GRACEFULL_SIGNAL_COUNTER_LIMIT,
                SCHEDULER_HEATH_SIGNAL_TIMEOUT_MILLI,
                NO_LOOP_DETECT_TIMEOUT_MILLI,
                watchdog_state,
                scheduler_health_rx,
            )
            .await;
        }
    });

    serve_files(bind_addr, watchdog_state).await?;
    Ok(scheduler_health_tx)
}

async fn run_watchdog(
    graceful_signal_counter_limit: usize,
    scheduler_health_signal_timeout: Duration,
    no_loop_detect_timeout: Duration,
    watchdog_state: Arc<Mutex<WatchDogState>>,
    mut scheduler_health_rx: mpsc::Receiver<HealthCheckSignal>,
) {
    let mut scheduler_state = (true, true, true);

    // Define a timer to detect when the mempool pick_task is never call but the loop is working.
    let mut no_loop_detect_timer = tokio::time::interval(no_loop_detect_timeout);
    let mut see_scheduler_loop_ok = true;

    let mut timeout_counter = 0;

    loop {
        let mut new_state = scheduler_state;
        select! {
             _ = no_loop_detect_timer.tick() => {
                // No loop ok since last call. Set scheduler loop issue
                if !see_scheduler_loop_ok {
                    new_state.0 = false
                }
                see_scheduler_loop_ok = false;
             }
            res = timeout(scheduler_heath_signal_timeout, scheduler_health_rx.recv()) => match res {
                Ok(res) => {
                    //The scheduler signal is alive
                    timout_counter = 0;
                    new_state.2 = true;
                    match res {
                        Some(msg) => match msg {
                            HealthCheckSignal::SchedulerLoopOk => {
                                see_scheduler_loop_ok = true;
                                new_state.0 = true
                            },
                            HealthCheckSignal::SchedulerMempoolLen(len) => new_state.1 = len == 0,
                        },
                        None => {
                            tracing::error!(
                                        "Scheduler Health channel error, Health channel closed. Stop health check."
                                    );
                            // Alone receiver won't get any message. recv always timeout.
                            (_, scheduler_health_rx) = tokio::sync::mpsc::channel(1);
                            new_state.2 = false;
                        }
                    }
                }
                Err(_) => {
                    tracing::error!("Scheduler Health signal Timeout, no health signal available.");
                    timout_counter += 1;
                    if timout_counter >= gracefull_signal_counter_limit {
                        new_state.2 = false;
                    }
                }
            }
        }

        let state_changed = new_state != scheduler_state;
        scheduler_state = new_state;

        //update watchdog state
        if state_changed {
            let mut watchdog_state = watchdog_state.lock().await;
            if !scheduler_state.0 || !scheduler_state.1 {
                tracing::warn!(
                    "watchdog detect critical state loop:{} mempool:{}",
                    scheduler_state.0,
                    scheduler_state.1
                );
                *watchdog_state = WatchDogState::Critical;
            } else if !scheduler_state.2 {
                tracing::warn!("watchdog detect Graceful issue.");
                *watchdog_state = WatchDogState::Graceful;
            } else {
                *watchdog_state = WatchDogState::Alive;
            }
        }
    }
}

// Start the local server and serve the watch dog requert.
// WatchDogState::Alive => 200
// WatchDogState::Graceful => 204
// WatchDogState::Critical => 503
async fn serve_healthcheck(
    bind_addr: SocketAddr,
    watchdog_state: Arc<Mutex<WatchDogState>>,
) -> Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;

    tokio::spawn({
        async move {
            tracing::info!(
                "Watchdog listening for http at {}",
                listener
                    .local_addr()
                    .expect("http listener's local address")
            );

            loop {
                match listener.accept().await {
                    Ok((stream, _from)) => {
                        let io = TokioIo::new(stream);
                        tokio::task::spawn({
                            let watchdog_state = watchdog_state.clone();
                            async move {
                                if let Err(err) = http1::Builder::new()
                                    .serve_connection(
                                        io,
                                        service_fn(|_| {
                                            let watchdog_state = watchdog_state.clone();
                                            async move {
                                                let status: StatusCode =
                                                    (*watchdog_state.lock().await).into();
                                                Response::builder()
                                                    .status(status)
                                                    .body(String::new())
                                            }
                                        }),
                                    )
                                    .await
                                {
                                    tracing::error!("Error serving watchdog connection: {err}.");
                                }
                            }
                        });
                    }
                    Err(err) => {
                        tracing::error!("Error during node connection to file http server:{err}");
                    }
                }
            }
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_watchdog() {
        let (scheduler_health_tx, scheduler_health_rx) = mpsc::channel::<HealthCheckSignal>(100);
        let watchdog_state = Arc::new(Mutex::new(WatchDogState::Alive));
        tokio::spawn({
            let watchdog_state = watchdog_state.clone();
            async move {
                run_watchdog(
                    2,
                    Duration::from_millis(100),
                    Duration::from_millis(300),
                    watchdog_state,
                    scheduler_health_rx,
                )
                .await;
            }
        });
        assert_eq!(WatchDogState::Alive, *watchdog_state.lock().await);
        scheduler_health_tx
            .send(HealthCheckSignal::SchedulerLoopOk)
            .await
            .unwrap();
        sleep(Duration::from_millis(20)).await;
        assert_eq!(WatchDogState::Alive, *watchdog_state.lock().await);
        scheduler_health_tx
            .send(HealthCheckSignal::SchedulerMempoolLen(0))
            .await
            .unwrap();
        sleep(Duration::from_millis(20)).await;
        assert_eq!(WatchDogState::Alive, *watchdog_state.lock().await);
        scheduler_health_tx
            .send(HealthCheckSignal::SchedulerMempoolLen(1))
            .await
            .unwrap();
        sleep(Duration::from_millis(20)).await;
        assert_eq!(WatchDogState::Critical, *watchdog_state.lock().await);
        //reset loop no detect time
        scheduler_health_tx
            .send(HealthCheckSignal::SchedulerLoopOk)
            .await
            .unwrap();
        //set state to Alive.
        scheduler_health_tx
            .send(HealthCheckSignal::SchedulerMempoolLen(0))
            .await
            .unwrap();
        sleep(Duration::from_millis(20)).await;
        assert_eq!(WatchDogState::Alive, *watchdog_state.lock().await);
        sleep(Duration::from_millis(150)).await;
        assert_eq!(WatchDogState::Alive, *watchdog_state.lock().await);
        sleep(Duration::from_millis(100)).await;
        assert_eq!(WatchDogState::Graceful, *watchdog_state.lock().await);
        scheduler_health_tx
            .send(HealthCheckSignal::SchedulerMempoolLen(0))
            .await
            .unwrap();
        sleep(Duration::from_millis(20)).await;
        assert_eq!(WatchDogState::Alive, *watchdog_state.lock().await);
        // Detect no loop
        sleep(Duration::from_millis(350)).await;
        assert_eq!(WatchDogState::Critical, *watchdog_state.lock().await);
        // Return to normal state.
        scheduler_health_tx
            .send(HealthCheckSignal::SchedulerLoopOk)
            .await
            .unwrap();
        sleep(Duration::from_millis(20)).await;
        assert_eq!(WatchDogState::Alive, *watchdog_state.lock().await);
    }
}
