use controllers::{
    controllers::{run, State},
    lock::LeaderLock,
    signals::SignalHandler,
};
use kube::Client;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Initiatilize Kubernetes controller state
    let state = State::default();
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");
    let mut lock = LeaderLock::new(Some("default".into())).await;
    let mut signal_handler = SignalHandler::new().expect("unable to create signal handler");
    let shutdown_channel = signal_handler.get_rx_channel();
    let mut shutdown = false;

    while !shutdown {
        let controllers = if lock.is_locked() {
            info!("Leader lock has bene acquired");
            Some(tokio::spawn(run(
                client.clone(),
                state.clone(),
                shutdown_channel.clone(),
            )))
        } else {
            None
        };

        tokio::select! {
            _ = signal_handler.shutdown_on_signal() => {
                shutdown = true;
            },
            _ = lock.wait_for_change(shutdown_channel.clone()) => {},
        }

        if let Some(controllers) = controllers {
            let _ = tokio::join!(controllers);
            info!("Leader lock has been lost");
        }
    }

    Ok(())
}
