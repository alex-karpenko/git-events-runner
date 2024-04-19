use crate::controllers::trigger::Trigger;
use crate::WebhookTriggerSpec;
use crate::{
    controllers::{State as AppState, TriggersState},
    WebhookTrigger,
};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::{
    extract::{FromRequest, Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use futures::Future;
use kube::{Api, Client};
use sacs::scheduler::{Scheduler, TaskScheduler};
use sacs::task::TaskId;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
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
}

#[derive(FromRequest, Serialize)]
#[from_request(via(axum::Json), rejection(AppError))]
struct AppResponseJson {
    status: AppResponseStatus,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum AppResponseStatus {
    Ok,
    Error,
}

#[derive(Display)]
enum AppError {
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
    mut shutdown: watch::Receiver<bool>,
    scheduler: Arc<RwLock<Scheduler>>,
    triggers_state: Arc<RwLock<TriggersState<WebhookTriggerSpec>>>,
    port: u16,
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
                let scheduler = Arc::into_inner(scheduler).unwrap().into_inner();
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
) -> Result<AppResponseJson, AppError> {
    warn!("webhook: get trigger hook isn't implemented");
    debug!("webhook: GET, namespace={namespace}, trigger={trigger}");
    Err(AppError::NotImplemented)
}

async fn handle_post_trigger_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger)): Path<(String, String)>,
    _headers: HeaderMap,
) -> Result<AppResponseJson, AppError> {
    debug!("webhook: POST, namespace={namespace}, trigger={trigger}");

    let triggers_api: Api<WebhookTrigger> = Api::namespaced(state.client.clone(), &namespace);
    let trigger = triggers_api.get(&trigger).await?;
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
        let task_id = scheduler.add(task).await?;

        Ok(task_id.into())
    } else {
        warn!("try to run forbidden multi-source hook on trigger {trigger_hash_key}");
        Err(AppError::ForbiddenMultiSource)
    }
}

async fn handle_get_source_webhook(
    State(_state): State<WebState>,
    Path((namespace, trigger, source)): Path<(String, String, String)>,
    _headers: HeaderMap,
) -> Result<AppResponseJson, AppError> {
    warn!("webhook: get source hook isn't implemented");
    debug!("webhook: GET, namespace={namespace}, trigger={trigger}, source={source}");
    Err(AppError::NotImplemented)
}

async fn handle_post_source_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger, source)): Path<(String, String, String)>,
    _headers: HeaderMap,
) -> Result<AppResponseJson, AppError> {
    debug!("webhook: POST, namespace={namespace}, trigger={trigger}, source={source}");

    let triggers_api: Api<WebhookTrigger> = Api::namespaced(state.client.clone(), &namespace);
    let trigger = triggers_api.get(&trigger).await?;
    let trigger_hash_key = trigger.trigger_hash_key();

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
        Err(AppError::SourceNotFound)
    }
}

impl IntoResponse for AppResponseJson {
    fn into_response(self) -> Response {
        (StatusCode::ACCEPTED, Json(self)).into_response()
    }
}

impl From<TaskId> for AppResponseJson {
    fn from(value: TaskId) -> Self {
        Self {
            status: AppResponseStatus::Ok,
            message: "job has been scheduled".into(),
            task_id: Some(value.to_string()),
        }
    }
}

impl From<String> for AppResponseJson {
    fn from(value: String) -> Self {
        Self {
            status: AppResponseStatus::Ok,
            message: value,
            task_id: None,
        }
    }
}

impl From<&str> for AppResponseJson {
    fn from(value: &str) -> Self {
        Self {
            status: AppResponseStatus::Ok,
            message: value.to_string(),
            task_id: None,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let resp = AppResponseJson {
            status: AppResponseStatus::Error,
            message: self.to_string(),
            task_id: None,
        };

        let status: StatusCode = match self {
            AppError::ForbiddenMultiSource => StatusCode::BAD_REQUEST,
            AppError::TriggerNotFound => StatusCode::NOT_FOUND,
            AppError::SourceNotFound => StatusCode::NOT_FOUND,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::KubeError { msg: _ } => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::SchedulerError { msg: _ } => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::NotImplemented => StatusCode::NOT_IMPLEMENTED,
        };

        (status, resp).into_response()
    }
}

impl From<kube::Error> for AppError {
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

impl From<sacs::Error> for AppError {
    fn from(value: sacs::Error) -> Self {
        Self::SchedulerError {
            msg: value.to_string(),
        }
    }
}
