use crate::controllers::trigger::Trigger;
use crate::WebhookTriggerSpec;
use crate::{
    controllers::{State as AppState, TriggersState},
    WebhookTrigger,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use futures::Future;
use kube::{Api, Client};
use sacs::scheduler::{Scheduler, TaskScheduler};
use std::sync::Arc;
use tokio::{
    net::TcpListener,
    sync::{watch, RwLock},
};
use tracing::{debug, error, info, warn};

pub const DEFAULT_HOOKS_WEB_BIND_ADDRESS: &str = "0.0.0.0:8080";
pub const DEFAULT_UTILS_WEB_BIND_ADDRESS: &str = "0.0.0.0:3000";

#[derive(Clone)]
struct WebState {
    scheduler: Arc<RwLock<Scheduler>>,
    triggers: Arc<RwLock<TriggersState<WebhookTriggerSpec>>>,
    client: Client,
}

pub async fn build_utils_web(
    app_state: AppState,
    mut shutdown: watch::Receiver<bool>,
) -> impl Future<Output = ()> {
    let app = Router::new()
        .route("/ready", get(handle_ready))
        .route("/alive", get(|| async { (StatusCode::OK, "Alive") }))
        .route("/metrics", get(handle_metrics))
        .with_state(app_state);
    let listener = TcpListener::bind(DEFAULT_UTILS_WEB_BIND_ADDRESS)
        .await
        .expect("unable to create Utility beb listener");
    let web = axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.changed().await.unwrap_or(()) });

    async move {
        info!("Starting Utility web server on {DEFAULT_UTILS_WEB_BIND_ADDRESS}");
        if let Err(err) = web.await {
            error!("Error while Utility web server running: {err}");
        }
        info!("Shutting down Utility web server");
    }
}

pub async fn build_hooks_web(
    mut shutdown: watch::Receiver<bool>,
    scheduler: Arc<RwLock<Scheduler>>,
    triggers_state: Arc<RwLock<TriggersState<WebhookTriggerSpec>>>,
) -> impl Future<Output = ()> {
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");

    let state = WebState {
        scheduler: scheduler.clone(),
        triggers: triggers_state,
        client,
    };

    let app = Router::new()
        .route(
            "/:namespace/:trigger",
            get(handle_get_trigger_webhook).post(handle_post_trigger_webhook),
        )
        .route(
            "/:namespace/:trigger/:source",
            get(handle_get_source_webhook).post(handle_post_source_webhook),
        )
        .with_state(state);
    let listener = TcpListener::bind(DEFAULT_HOOKS_WEB_BIND_ADDRESS)
        .await
        .expect("unable to create Webhooks listener");
    let web = axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.changed().await.unwrap_or(()) });

    async move {
        info!("Starting Hooks web server on {DEFAULT_HOOKS_WEB_BIND_ADDRESS}");
        let res = web.await;

        if Arc::strong_count(&scheduler) <= 1 {
            info!("Shutting down Webhooks task scheduler");
            let scheduler = Arc::into_inner(scheduler).unwrap().into_inner();
            scheduler
                .shutdown(sacs::scheduler::ShutdownOpts::WaitForFinish)
                .await
                .unwrap_or(());
        }

        if let Err(err) = res {
            error!("Error while Webhooks server running: {err}");
        }
        info!("Shutting down Webhooks server");
    }
}

async fn handle_ready(State(state): State<AppState>) -> (StatusCode, &'static str) {
    if *state.ready.read().await {
        debug!("utility web: ready");
        (StatusCode::OK, "Ready")
    } else {
        debug!("utility web: not ready");
        (StatusCode::INTERNAL_SERVER_ERROR, "Not ready")
    }
}

async fn handle_metrics(State(_state): State<AppState>) -> (StatusCode, String) {
    warn!("utility web: metrics endpoint isn't implemented");
    (StatusCode::NOT_IMPLEMENTED, "Not implemented".into())
}

async fn handle_get_trigger_webhook(
    State(_state): State<WebState>,
    Path((namespace, trigger)): Path<(String, String)>,
) -> (StatusCode, &'static str) {
    warn!("webhook: get trigger hook isn't implemented");
    warn!("webhook: GET, namespace={namespace}, trigger={trigger}");
    (StatusCode::NOT_IMPLEMENTED, "Not implemented")
}

async fn handle_post_trigger_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger)): Path<(String, String)>,
) -> (StatusCode, Json<serde_json::Value>) {
    debug!("webhook: POST, namespace={namespace}, trigger={trigger}");

    let triggers_api: Api<WebhookTrigger> = Api::namespaced(state.client.clone(), &namespace);
    match triggers_api.get(&trigger).await {
        Ok(trigger) => {
            let trigger_hash_key = trigger.trigger_hash_key();
            if trigger.spec.webhook.multi_source {
                info!("Run all sources task for trigger {trigger_hash_key}");
                let task = trigger.create_trigger_task(
                    state.client.clone(),
                    sacs::task::TaskSchedule::Once,
                    None,
                    state.triggers.clone(),
                );
                let scheduler = state.scheduler.write().await;
                let mut triggers = state.triggers.write().await;
                let task_id = scheduler.add(task).await.unwrap(); // TODO: get rid of unwrap
                let tasks = &mut triggers.tasks;
                tasks.insert(trigger_hash_key, task_id);

                (
                    StatusCode::ACCEPTED,
                    Json(serde_json::json!({"status": "ok", "message": "Accepted"})),
                )
            } else {
                warn!("try to run multi-source hook on trigger {trigger_hash_key}");
                (
                    StatusCode::BAD_REQUEST,
                    Json(
                        serde_json::json!({"status": "error", "message": "Multi-source hooks isn't allowed"}),
                    ),
                )
            }
        }
        Err(_err) => todo!(),
    }
}

async fn handle_get_source_webhook(
    State(_state): State<WebState>,
    Path((namespace, trigger, source)): Path<(String, String, String)>,
) -> (StatusCode, &'static str) {
    warn!("webhook: get source hook isn't implemented");
    warn!("webhook: GET, namespace={namespace}, trigger={trigger}, source={source}");
    (StatusCode::NOT_IMPLEMENTED, "Not implemented")
}

async fn handle_post_source_webhook(
    State(_state): State<WebState>,
    Path((namespace, trigger, source)): Path<(String, String, String)>,
) -> (StatusCode, &'static str) {
    warn!("webhook: post source hook isn't implemented");
    warn!("webhook: POST, namespace={namespace}, trigger={trigger}, source={source}");
    (StatusCode::NOT_IMPLEMENTED, "Not implemented")
}
