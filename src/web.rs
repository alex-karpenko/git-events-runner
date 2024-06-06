use crate::cache::{ApiCache, SecretsCache};
use crate::config::RuntimeConfig;
use crate::controller::State as AppState;
use crate::resources::trigger::{Trigger, TriggerTaskSources, WebhookTrigger, WebhookTriggerSpec};
use crate::Error;
use axum::routing::post;
use axum::{
    extract::{FromRequest, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures::Future;
use kube::Client;
use sacs::{
    scheduler::{Scheduler, TaskScheduler},
    task::TaskId,
};
use serde::Serialize;
use std::{sync::Arc, time::Duration};
use strum::Display;
use tokio::{
    net::TcpListener,
    sync::{watch, RwLock},
};
use tracing::{debug, error, info, warn};

/// State is attached to each web request
#[derive(Clone)]
struct WebState {
    client: Client,                    // we need it to call Trigger::create_trigger_task()
    scheduler: Arc<RwLock<Scheduler>>, // to post tasks, with Once schedule
    source_clone_folder: String,       // config to pass to Trigger::create_trigger_task()
}

/// Struct to convert it to response using set of From trait implementations.
/// We return JSON with predefined status (ok/err/...) and mandatory message
/// which explains status
/// task_id is for successful calls to triggers
#[derive(FromRequest, Serialize)]
#[from_request(via(axum::Json), rejection(WebError))]
struct WebResponseJson {
    status: WebResponseStatus,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum WebResponseStatus {
    Ok,
    Error,
}

#[derive(Display)]
enum WebError {
    #[strum(to_string = "multi-source hook isn't allowed")]
    ForbiddenMultiSource,
    #[strum(to_string = "trigger doesn't exist")]
    TriggerNotFound,
    #[strum(to_string = "source doesn't exist in the trigger")]
    SourceNotFound,
    #[strum(to_string = "authorization is required")]
    Unauthorized,
    #[strum(to_string = "forbidden")]
    Forbidden,
    #[strum(to_string = "unable to complete authorization")]
    AuthorizationError,
    #[strum(to_string = "KubeError: {msg}")]
    KubeError { msg: String },
    #[strum(to_string = "TaskScheduler: {msg}")]
    SchedulerError { msg: String },
    #[strum(to_string = "Unknown: {msg}")]
    UnknownError { msg: String },
}

/// Returns web server Future with endpoints for health/live checks and metrics responder.
/// It exits by receiving anything from `shutdown` channel.
/// TODO: Use state to store metrics
pub async fn build_utils_web(
    app_state: AppState,
    mut shutdown: watch::Receiver<bool>,
    port: u16,
) -> impl Future<Output = ()> {
    let listen_to = format!("0.0.0.0:{port}");
    let app = Router::new()
        .route("/ready", get(handle_ready))
        .route("/alive", get(|| async { (StatusCode::OK, "Alive") }))
        .route("/metrics", get(handle_metrics))
        .with_state(app_state);
    let listener = TcpListener::bind(listen_to.clone())
        .await
        .expect("unable to create Utility beb listener");
    let web = axum::serve(listener, app).with_graceful_shutdown(async move { shutdown.changed().await.unwrap_or(()) });

    async move {
        info!("Starting Utility web server on {listen_to}");
        if let Err(err) = web.await {
            error!("Error while Utility web server running: {err}");
        }
        info!("Shutting down Utility web server");
    }
}

/// Returns webhooks server Future with two base endpoints:
/// - all sources trigger: /namespace/trigger
/// - single source trigger: /namespace/trigger/source
/// It exits by receiving anything from `shutdown` channel.
pub async fn build_hooks_web(
    client: Client,
    mut shutdown: watch::Receiver<bool>,
    scheduler: Arc<RwLock<Scheduler>>,
    port: u16,
    source_clone_folder: String,
) -> impl Future<Output = ()> {
    let state = WebState {
        scheduler: scheduler.clone(),
        client,
        source_clone_folder,
    };

    let listen_to = format!("0.0.0.0:{port}");
    let app = Router::new()
        .route("/:namespace/:trigger", post(handle_post_trigger_webhook))
        .route("/:namespace/:trigger/:source", post(handle_post_source_webhook))
        .with_state(state);
    let listener = TcpListener::bind(listen_to.clone())
        .await
        .expect("unable to create Webhooks listener");
    let web = axum::serve(listener, app).with_graceful_shutdown(async move { shutdown.changed().await.unwrap_or(()) });

    async move {
        info!("Starting Hooks web server on {listen_to}");
        if let Err(err) = web.await {
            error!("Error while Webhooks server running: {err}");
        }

        // To shutdown task scheduler we have to extract it from shared reference (Arc), but
        // sometimes controller works few milliseconds longer than expected,
        // just wait few async-state-machine cycles to finish
        for _ in 0..10 {
            if Arc::strong_count(&scheduler) <= 1 {
                info!("Shutting down WebhookTriggers task scheduler");
                let scheduler = Arc::into_inner(scheduler)
                    .expect("more than one copies of scheduler is present, looks like a BUG!")
                    .into_inner();
                scheduler
                    .shutdown(sacs::scheduler::ShutdownOpts::WaitForFinish)
                    .await
                    .unwrap_or(());
                break;
            } else {
                debug!("Webhooks task scheduler is in use, waiting...");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        info!("Shutting down Webhooks server");
    }
}

async fn handle_ready(State(state): State<AppState>) -> (StatusCode, &'static str) {
    // depends on global readiness state
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

/// Trigger jobs for all sources of the trigger
async fn handle_post_trigger_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<WebResponseJson, WebError> {
    debug!("webhook: POST, namespace={namespace}, trigger={trigger}");

    let trigger = WebhookTrigger::get(&trigger, Some(&namespace))?;
    let trigger_hash_key = trigger.trigger_hash_key();

    state
        .check_hook_authorization(&headers, &trigger.spec, &namespace)
        .await?;
    if trigger.spec.webhook.multi_source {
        info!("Run all sources task for trigger {trigger_hash_key}");
        let task = trigger.create_trigger_task(
            state.client.clone(),
            sacs::task::TaskSchedule::Once,
            TriggerTaskSources::All,
            state.source_clone_folder,
        );
        let scheduler = state.scheduler.write().await;
        let task_id = scheduler.add(task).await?;

        Ok(task_id.into()) // include task_id into successful response
    } else {
        warn!("Try to run forbidden multi-source hook on trigger {trigger_hash_key}");
        Err(WebError::ForbiddenMultiSource)
    }
}

/// Single source trigger run
async fn handle_post_source_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger, source)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<WebResponseJson, WebError> {
    debug!("webhook: POST, namespace={namespace}, trigger={trigger}, source={source}");

    let trigger = WebhookTrigger::get(&trigger, Some(&namespace))?;
    let trigger_hash_key = trigger.trigger_hash_key();

    state
        .check_hook_authorization(&headers, &trigger.spec, &namespace)
        .await?;
    if trigger.spec.sources.names.contains(&source) {
        info!("Run source task {trigger_hash_key}/{source}");
        let task = trigger.create_trigger_task(
            state.client.clone(),
            sacs::task::TaskSchedule::Once,
            TriggerTaskSources::Single(source.clone()), // by specifying single source we restrict scope of task
            state.source_clone_folder,
        );
        let scheduler = state.scheduler.write().await;
        let task_id = scheduler.add(task).await?;

        Ok(task_id.into())
    } else {
        warn!("source `{source}` doesn't exist in trigger {trigger_hash_key}");
        Err(WebError::SourceNotFound)
    }
}

impl IntoResponse for WebResponseJson {
    fn into_response(self) -> Response {
        (StatusCode::ACCEPTED, Json(self)).into_response()
    }
}

impl From<TaskId> for WebResponseJson {
    fn from(value: TaskId) -> Self {
        Self {
            status: WebResponseStatus::Ok,
            message: "job has been scheduled".into(),
            task_id: Some(value.to_string()),
        }
    }
}

impl From<String> for WebResponseJson {
    fn from(value: String) -> Self {
        Self {
            status: WebResponseStatus::Ok,
            message: value,
            task_id: None,
        }
    }
}

impl From<&str> for WebResponseJson {
    fn from(value: &str) -> Self {
        Self {
            status: WebResponseStatus::Ok,
            message: value.to_string(),
            task_id: None,
        }
    }
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let resp = WebResponseJson {
            status: WebResponseStatus::Error,
            message: self.to_string(),
            task_id: None,
        };

        let status: StatusCode = match self {
            WebError::ForbiddenMultiSource => StatusCode::BAD_REQUEST,
            WebError::TriggerNotFound => StatusCode::NOT_FOUND,
            WebError::SourceNotFound => StatusCode::NOT_FOUND,
            WebError::Unauthorized => StatusCode::UNAUTHORIZED,
            WebError::Forbidden => StatusCode::FORBIDDEN,
            WebError::KubeError { msg: _ } => StatusCode::INTERNAL_SERVER_ERROR,
            WebError::SchedulerError { msg: _ } => StatusCode::INTERNAL_SERVER_ERROR,
            WebError::AuthorizationError => StatusCode::INTERNAL_SERVER_ERROR,
            WebError::UnknownError { msg: _ } => StatusCode::INTERNAL_SERVER_ERROR,
        };

        (status, resp).into_response()
    }
}

impl From<kube::Error> for WebError {
    fn from(value: kube::Error) -> Self {
        match &value {
            kube::Error::Api(err) => {
                if err.code == 404 {
                    Self::TriggerNotFound
                } else {
                    Self::KubeError { msg: err.to_string() }
                }
            }
            _ => Self::KubeError { msg: value.to_string() },
        }
    }
}

impl From<sacs::Error> for WebError {
    fn from(value: sacs::Error) -> Self {
        Self::SchedulerError { msg: value.to_string() }
    }
}

impl From<Error> for WebError {
    fn from(value: Error) -> Self {
        match value {
            Error::ResourceNotFoundError(_) => Self::TriggerNotFound,
            e => Self::UnknownError { msg: e.to_string() },
        }
    }
}

impl WebState {
    /// Check is webhook requires authorization
    /// If so - verify auth header
    /// Returns empty result if good or error to convert in web response if no
    async fn check_hook_authorization(
        &self,
        request_headers: &HeaderMap,
        webhook_spec: &WebhookTriggerSpec,
        namespace: &str,
    ) -> Result<(), WebError> {
        if let Some(auth_config) = &webhook_spec.webhook.auth_config {
            if let Some(header) = request_headers.get(
                auth_config
                    .header
                    .clone()
                    .unwrap_or(RuntimeConfig::get().trigger.webhook.default_auth_header.clone()),
            ) {
                // try to get secret from cache
                // let secret = self
                //     .secrets_cache
                //     .as_ref()
                let secret = SecretsCache::get(namespace, &auth_config.secret_ref.name, &auth_config.key)
                    .await
                    .map_err(|_| WebError::AuthorizationError)?; // something went wrong during interaction with secrets cache
                if *secret == *header {
                    Ok(()) // hit!
                } else {
                    Err(WebError::Forbidden) // header is present but provided value is incorrect
                }
            } else {
                Err(WebError::Unauthorized) // auth is required by header isn't present in the request
            }
        } else {
            Ok(()) // no need to auth
        }
    }
}
