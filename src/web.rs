use crate::controllers::State as AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Router,
};
use futures::Future;
use tokio::{net::TcpListener, sync::watch};
use tracing::{error, info, warn};

pub const DEFAULT_HOOKS_WEB_BIND_ADDRESS: &str = "0.0.0.0:8080";
pub const DEFAULT_UTILS_WEB_BIND_ADDRESS: &str = "0.0.0.0:3000";

pub async fn build_utils_web(
    state: AppState,
    mut shutdown: watch::Receiver<bool>,
) -> impl Future<Output = ()> {
    let app = Router::new()
        .route("/", get(handle_root))
        .route("/ready", get(handle_ready))
        .route("/alive", get(|| async { (StatusCode::OK, "Alive") }))
        .route("/metrics", get(handle_metrics))
        .with_state(state);
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
    state: AppState,
    mut shutdown: watch::Receiver<bool>,
) -> impl Future<Output = ()> {
    let app = Router::new()
        .route("/:namespace/:trigger", get(handle_webhook_all_sources))
        .route(
            "/:namespace/:trigger/:source",
            get(handle_webhook_single_source),
        )
        .with_state(state);
    let listener = TcpListener::bind(DEFAULT_HOOKS_WEB_BIND_ADDRESS)
        .await
        .expect("unable to create Webhooks listener");
    let web = axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.changed().await.unwrap_or(()) });

    async move {
        info!("Starting Webhooks server on {DEFAULT_UTILS_WEB_BIND_ADDRESS}");
        if let Err(err) = web.await {
            error!("Error while Webhooks server running: {err}");
        }
        info!("Shutting down Webhooks server");
    }
}

async fn handle_root(State(_state): State<AppState>) -> (StatusCode, String) {
    (StatusCode::NOT_IMPLEMENTED, "Not implemented".into())
}

async fn handle_ready(State(state): State<AppState>) -> (StatusCode, &'static str) {
    if *state.ready.read().await {
        (StatusCode::OK, "Ready")
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Not ready")
    }
}

async fn handle_metrics(State(_state): State<AppState>) -> (StatusCode, String) {
    (StatusCode::NOT_IMPLEMENTED, "Not implemented".into())
}

async fn handle_webhook_all_sources(
    State(_state): State<AppState>,
    Path((namespace, trigger)): Path<(String, String)>,
) -> (StatusCode, &'static str) {
    warn!("namespace={namespace}, trigger={trigger}");
    (StatusCode::NOT_IMPLEMENTED, "Not implemented")
}

async fn handle_webhook_single_source(
    State(_state): State<AppState>,
    Path((namespace, trigger, source)): Path<(String, String, String)>,
) -> (StatusCode, &'static str) {
    warn!("namespace={namespace}, trigger={trigger}, source={source}");
    (StatusCode::NOT_IMPLEMENTED, "Not implemented")
}
