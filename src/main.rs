use controllers::{
    controllers::{run_leader_controllers, State},
    lock,
    signals::SignalHandler,
};
use tokio::sync::watch;
use tracing::{error, info};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let state = State::default();
    let identity = Uuid::new_v4().to_string();
    let (mut lock_channel, lock_task) = lock::new(&identity, Some("default".into())).await;
    let mut signal_handler = SignalHandler::new().expect("unable to create signal handler");
    let mut shutdown = false;

    while !shutdown {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let is_leader = lock_channel.borrow_and_update().is_current_for(&identity);
        let controllers = if is_leader {
            info!("Leader lock has been acquired, identity={identity}");
            Some(tokio::spawn(run_leader_controllers(
                state.clone(),
                shutdown_rx.clone(),
            )))
        } else {
            None
        };

        tokio::select! {
            _ = signal_handler.wait_for_signal() => {
                    shutdown = true;
                },
            _ = async {
                    while lock_channel.borrow_and_update().is_current_for(&identity) == is_leader {
                        if let Err(err) = lock_channel.changed().await {
                            error!("Error while process leader lock changes: {err}");
                            shutdown = true;
                        }
                    }
                } => {},
        }

        if let Some(controllers) = controllers {
            if let Err(err) = shutdown_tx.send(true) {
                error!("Unable to send shutdown request: {err}");
            }
            let _ = tokio::join!(controllers);
            info!("Leader lock has been released");
        }
    }

    drop(lock_channel);
    let _ = tokio::join!(lock_task);

    Ok(())
}
