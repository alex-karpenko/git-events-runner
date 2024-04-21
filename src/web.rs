use crate::controller::{State as AppState, TriggersState};
use crate::resources::trigger::{Trigger, WebhookTrigger, WebhookTriggerSpec};
use crate::secrets_cache::{ExpiringSecretCache, SecretCache};
use axum::{
    extract::{FromRequest, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use futures::Future;
use kube::{Api, Client};
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

#[derive(Clone)]
struct WebState {
    scheduler: Arc<RwLock<Scheduler>>,
    triggers: Arc<RwLock<TriggersState<WebhookTriggerSpec>>>,
    client: Client,
    secrets_cache: Arc<ExpiringSecretCache>,
}

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
    #[strum(to_string = "method isn't not implemented")]
    NotImplemented,
    #[strum(to_string = "TaskScheduler: {msg}")]
    SchedulerError { msg: String },
}

pub async fn build_utils_web(
    app_state: AppState,
    mut shutdown: watch::Receiver<bool>,
    port: u16,
) -> impl Future<Output = ()> {
    let app = Router::new()
        .route("/ready", get(handle_ready))
        .route("/alive", get(|| async { (StatusCode::OK, "Alive") }))
        .route("/metrics", get(handle_metrics))
        .with_state(app_state);
    let listener = TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("unable to create Utility beb listener");
    let web = axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.changed().await.unwrap_or(()) });

    async move {
        info!("Starting Utility web server on 0.0.0.0:{port}");
        if let Err(err) = web.await {
            error!("Error while Utility web server running: {err}");
        }
        info!("Shutting down Utility web server");
    }
}

pub async fn build_hooks_web(
    client: Client,
    mut shutdown: watch::Receiver<bool>,
    scheduler: Arc<RwLock<Scheduler>>,
    triggers_state: Arc<RwLock<TriggersState<WebhookTriggerSpec>>>,
    secrets_cache: Arc<ExpiringSecretCache>,
    port: u16,
) -> impl Future<Output = ()> {
    let state = WebState {
        scheduler: scheduler.clone(),
        triggers: triggers_state,
        client,
        secrets_cache,
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
    let listener = TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .expect("unable to create Webhooks listener");
    let web = axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.changed().await.unwrap_or(()) });

    async move {
        info!("Starting Hooks web server on 0.0.0.0:{port}");
        if let Err(err) = web.await {
            error!("Error while Webhooks server running: {err}");
        }

        // Sometimes controller works few milliseconds longer than expected, just wait few state-machine cycles to finish
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
    _headers: HeaderMap,
) -> Result<WebResponseJson, WebError> {
    warn!("webhook: get trigger hook isn't implemented");
    debug!("webhook: GET, namespace={namespace}, trigger={trigger}");

    Err(WebError::NotImplemented)
}

async fn handle_post_trigger_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<WebResponseJson, WebError> {
    debug!("webhook: POST, namespace={namespace}, trigger={trigger}");

    let triggers_api: Api<WebhookTrigger> = Api::namespaced(state.client.clone(), &namespace);
    let trigger = triggers_api.get(&trigger).await?;
    let trigger_hash_key = trigger.trigger_hash_key();

    state
        .check_hook_authorization(&headers, &trigger.spec, &namespace)
        .await?;
    if trigger.spec.webhook.multi_source {
        info!("Run all sources task for trigger {trigger_hash_key}");
        let task = trigger.create_trigger_task(
            state.client.clone(),
            sacs::task::TaskSchedule::Once,
            None,
            state.triggers.clone(),
        );
        let scheduler = state.scheduler.write().await;
        let task_id = scheduler.add(task).await?;

        Ok(task_id.into())
    } else {
        warn!("try to run forbidden multi-source hook on trigger {trigger_hash_key}");
        Err(WebError::ForbiddenMultiSource)
    }
}

async fn handle_get_source_webhook(
    State(_state): State<WebState>,
    Path((namespace, trigger, source)): Path<(String, String, String)>,
    _headers: HeaderMap,
) -> Result<WebResponseJson, WebError> {
    warn!("webhook: get source hook isn't implemented");
    debug!("webhook: GET, namespace={namespace}, trigger={trigger}, source={source}");

    Err(WebError::NotImplemented)
}

async fn handle_post_source_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger, source)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<WebResponseJson, WebError> {
    debug!("webhook: POST, namespace={namespace}, trigger={trigger}, source={source}");

    let triggers_api: Api<WebhookTrigger> = Api::namespaced(state.client.clone(), &namespace);
    let trigger = triggers_api.get(&trigger).await?;
    let trigger_hash_key = trigger.trigger_hash_key();

    state
        .check_hook_authorization(&headers, &trigger.spec, &namespace)
        .await?;
    if trigger.spec.sources.names.contains(&source) {
        info!("Run source task {trigger_hash_key}/{source}");
        let task = trigger.create_trigger_task(
            state.client.clone(),
            sacs::task::TaskSchedule::Once,
            Some(source.clone()),
            state.triggers.clone(),
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
            WebError::NotImplemented => StatusCode::NOT_IMPLEMENTED,
            WebError::AuthorizationError => StatusCode::INTERNAL_SERVER_ERROR,
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
                    Self::KubeError {
                        msg: err.to_string(),
                    }
                }
            }
            _ => Self::KubeError {
                msg: value.to_string(),
            },
        }
    }
}

impl From<sacs::Error> for WebError {
    fn from(value: sacs::Error) -> Self {
        Self::SchedulerError {
            msg: value.to_string(),
        }
    }
}

impl WebState {
    async fn check_hook_authorization(
        &self,
        request_headers: &HeaderMap,
        webhook_spec: &WebhookTriggerSpec,
        namespace: &str,
    ) -> Result<(), WebError> {
        if let Some(auth_config) = &webhook_spec.webhook.auth_config {
            if let Some(header) = request_headers.get(&auth_config.header) {
                let secret = self
                    .secrets_cache
                    .as_ref()
                    .get(namespace, &auth_config.secret_ref.name, &auth_config.key)
                    .await
                    .map_err(|_| WebError::AuthorizationError)?;
                if *secret == *header {
                    Ok(())
                } else {
                    Err(WebError::Forbidden)
                }
            } else {
                Err(WebError::Unauthorized)
            }
        } else {
            Ok(())
        }
    }
}
