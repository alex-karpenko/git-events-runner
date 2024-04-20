use controllers::{
    cli::Cli,
    controllers::{run_leader_controllers, run_web_controllers, State, TriggersState},
    lock,
    secret_cache::ExpiringSecretCache,
    signals::SignalHandler,
    web,
};
use kube::Client;
use sacs::scheduler::Scheduler;
use std::{sync::Arc, time::Duration};
use tokio::sync::{watch, RwLock};
use tracing::{error, info};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::new();

    let state = State::default();
    let client = Client::try_default().await?;
    let secrets_cache = ExpiringSecretCache::new(Duration::from_secs(30), client.clone());
    let identity = Uuid::new_v4().to_string();
    let (mut lock_channel, lock_task) = lock::new(&identity, Some("default".into())).await;
    let mut signal_handler = SignalHandler::new().expect("unable to create signal handler");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut shutdown = false;

    let utils_web =
        web::build_utils_web(state.clone(), shutdown_rx.clone(), cli.utility_port).await;
    let utils_web = tokio::spawn(utils_web);

    let (hooks_web, hooks_controller) = {
        let scheduler = Arc::new(RwLock::new(Scheduler::default()));
        let triggers_state = Arc::new(RwLock::new(TriggersState::default()));
        let hooks_controller = tokio::spawn(run_web_controllers(
            client.clone(),
            state.clone(),
            shutdown_rx.clone(),
            scheduler.clone(),
            triggers_state.clone(),
        ));
        let hooks_web = web::build_hooks_web(
            client.clone(),
            shutdown_rx.clone(),
            scheduler,
            triggers_state,
            secrets_cache.clone(),
            cli.webhooks_port,
        )
        .await;
        (hooks_web, hooks_controller)
    };
    let hooks_web = tokio::spawn(hooks_web);

    while !shutdown {
        let (changed_tx, changed_rx) = watch::channel(false);
        let is_leader = lock_channel.borrow_and_update().is_current_for(&identity);
        let controllers = if is_leader {
            info!("Leader lock has been acquired, identity={identity}");
            Some(tokio::spawn(run_leader_controllers(
                client.clone(),
                state.clone(),
                changed_rx.clone(),
            )))
        } else {
            None
        };

        {
            let mut ready = state.ready.write().await;
            *ready = true;
        }

        tokio::select! {
            _ = signal_handler.wait_for_signal() => {
                    if let Err(err) = shutdown_tx.send(true) {
                        error!("Error while sending shutdown event: {err}");
                    }
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
            if let Err(err) = changed_tx.send(true) {
                error!("Unable to send change event: {err}");
            }
            let _ = tokio::join!(controllers);
            info!("Leader lock has been released");
        }
    }

    drop(lock_channel);
    let _ = tokio::join!(lock_task, utils_web, hooks_web, hooks_controller);

    Ok(())
}
