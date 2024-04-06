use controllers::{
    controllers::{run, State},
    signals::SignalHandler,
};
use kube::Client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Initiatilize Kubernetes controller state
    let mut signal_handler = SignalHandler::new().expect("unable to create signal handler");
    let state = State::default();
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");
        let mut signal_handler = SignalHandler::new().expect("unable to create signal handler");
        let controllers = run(
            client.clone(),
            state.clone(),
            signal_handler.get_rx_channel(),
        );

        tokio::join!(controllers, signal_handler.shutdown_on_signal());
    Ok(())
}
