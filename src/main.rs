use controllers::controllers::{run, State};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Initiatilize Kubernetes controller state
    let state = State::default();
    let controllers = run(state);

    tokio::join!(controllers);
    Ok(())
}
