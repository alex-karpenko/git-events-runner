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
use tracing::{debug, error, info, instrument, trace, warn};

/// State is attached to each web request
#[derive(Clone)]
struct WebState {
    client: Client,                    // we need it to call Trigger::create_trigger_task()
    scheduler: Arc<RwLock<Scheduler>>, // to post tasks, with Once schedule
    source_clone_folder: String,       // config to pass to Trigger::create_trigger_task()
}

/// Struct to convert it to a response using a set of From trait implementations.
/// We return JSON with predefined status (ok/err/...) and mandatory message
/// which explains status
/// task_id is for successful calls to triggers
#[derive(FromRequest, Serialize, Debug)]
#[from_request(via(axum::Json), rejection(WebError))]
struct WebResponseJson {
    status: WebResponseStatus,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
enum WebResponseStatus {
    Ok,
    Error,
}

#[derive(Display, Debug, PartialEq)]
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
        info!(address = %listen_to, "starting Utility web server");
        if let Err(err) = web.await {
            error!(error = %err, "running Utility web server");
        }
        info!("shutting down Utility web server");
    }
}

/// Returns webhooks server Future with two base endpoints:
///   - all sources trigger: /namespace/trigger;
///   - single source trigger: /namespace/trigger/source.
///
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
        info!(address = %listen_to, "starting Hooks web server");
        if let Err(err) = web.await {
            error!(error = %err, "running Webhooks server");
        }

        // To shut down task scheduler, we have to extract it from shared reference (Arc), but
        // sometimes the controller works few milliseconds longer than expected,
        // so wait for few async-state-machine cycles to finish
        for _ in 0..10 {
            if Arc::strong_count(&scheduler) <= 1 {
                info!("shutting down WebhookTriggers task scheduler");
                let scheduler = Arc::into_inner(scheduler)
                    .expect("more than one copies of scheduler is present, looks like a BUG!")
                    .into_inner();
                scheduler
                    .shutdown(sacs::scheduler::ShutdownOpts::WaitForFinish)
                    .await
                    .unwrap_or(());
                break;
            } else {
                debug!("webhooks task scheduler is in use, waiting...");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }

        info!("shutting down Webhooks server");
    }
}

#[instrument("ready", level = "trace", skip_all)]
async fn handle_ready(State(state): State<AppState>) -> (StatusCode, &'static str) {
    // depends on global readiness state
    let ready = *state.ready.read().await;
    trace!(%ready, "readiness requested");
    if ready {
        (StatusCode::OK, "Ready")
    } else {
        (StatusCode::INTERNAL_SERVER_ERROR, "Not ready")
    }
}

#[instrument("metrics", level = "trace", skip_all)]
async fn handle_metrics(State(_state): State<AppState>) -> (StatusCode, String) {
    warn!("metrics endpoint isn't implemented");
    (StatusCode::NOT_IMPLEMENTED, "Not implemented".into())
}

/// Trigger jobs for all sources of the trigger
#[instrument("trigger webhook", skip_all, fields(namespace, trigger))]
async fn handle_post_trigger_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<WebResponseJson, WebError> {
    trace!(%namespace, %trigger, "POST request");

    let trigger = WebhookTrigger::get(&trigger, Some(&namespace))?;
    let trigger_hash_key = trigger.trigger_hash_key();

    state
        .check_hook_authorization(&headers, &trigger.spec, &namespace)
        .await?;
    if trigger.spec.webhook.multi_source {
        info!(trigger = %trigger_hash_key, "running all-sources check");
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
        warn!(trigger = %trigger_hash_key, "multi-source hook is forbidden");
        Err(WebError::ForbiddenMultiSource)
    }
}

/// Single source trigger run
#[instrument("source webhook", skip_all, fields(namespace, trigger, source))]
async fn handle_post_source_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger, source)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<WebResponseJson, WebError> {
    trace!(%namespace, %trigger, %source, "POST request");

    let trigger = WebhookTrigger::get(&trigger, Some(&namespace))?;
    let trigger_hash_key = trigger.trigger_hash_key();

    state
        .check_hook_authorization(&headers, &trigger.spec, &namespace)
        .await?;
    if trigger.spec.sources.names.contains(&source) {
        info!(trigger = %trigger_hash_key, %source, "running single-source check");
        let task = trigger.create_trigger_task(
            state.client.clone(),
            sacs::task::TaskSchedule::Once,
            TriggerTaskSources::Single(source.clone()), // by specifying `single source` we restrict scope of the task.
            state.source_clone_folder,
        );
        let scheduler = state.scheduler.write().await;
        let task_id = scheduler.add(task).await?;

        Ok(task_id.into())
    } else {
        warn!(trigger = %trigger_hash_key, %source, "source doesn't exist in the trigger");
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
            e => {
                if let Error::KubeError(e) = e {
                    match e {
                        kube::Error::Api(err) => {
                            if err.code == 404 {
                                Self::TriggerNotFound
                            } else {
                                Self::KubeError { msg: err.to_string() }
                            }
                        }
                        _ => Self::KubeError { msg: e.to_string() },
                    }
                } else {
                    Self::UnknownError { msg: e.to_string() }
                }
            }
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
                    .map_err(|_| WebError::AuthorizationError)?; // something went wrong during interaction with secret cache
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::controller;
    use axum::extract::State;
    use axum::http::HeaderValue;
    use k8s_openapi::api::core::v1::{Namespace, Secret};
    use kube::api::{DeleteParams, PostParams};
    use kube::{Api, Resource};
    use tokio::sync::OnceCell;

    static INITIALIZED: OnceCell<()> = OnceCell::const_new();

    const NAMESPACE: &str = "web-webhook-triggers";
    const SECRET_NAME: &str = "webhook-triggers-auth-secret";
    const SECRET_TOKEN: &str = "0123456789";

    async fn init() -> WebState {
        INITIALIZED
            .get_or_init(|| async {
                let client = Client::try_default().await.unwrap();
                create_namespace(client.clone()).await;
                create_secret(client.clone()).await;
            })
            .await;

        get_test_web_state().await
    }

    /// Unattended namespace creation
    async fn create_namespace(client: Client) {
        let api = Api::<Namespace>::all(client);
        let pp = PostParams::default();

        let mut data = Namespace::default();
        data.meta_mut().name = Some(String::from(NAMESPACE));

        api.create(&pp, &data).await.unwrap_or_default();
    }

    /// Unattended auth secret creation
    async fn create_secret(client: Client) {
        let api = Api::<Secret>::namespaced(client, NAMESPACE);
        let pp = PostParams::default();

        let mut secrets = BTreeMap::new();
        secrets.insert("token".into(), k8s_openapi::ByteString(Vec::<u8>::from(SECRET_TOKEN)));

        let mut data = Secret::default();
        data.meta_mut().name = Some(String::from(SECRET_NAME));
        let _ = data.data.insert(secrets);

        api.create(&pp, &data).await.unwrap_or_default();
    }

    /// Create the simplest test web state
    async fn get_test_web_state() -> WebState {
        let client = Client::try_default().await.unwrap();
        let scheduler = Arc::new(RwLock::new(Scheduler::default()));

        WebState {
            client,
            scheduler,
            source_clone_folder: String::from("/tmp/test_git_events_runner_context"),
        }
    }

    /// Create the simplest test web state
    async fn get_test_controller_state() -> controller::State {
        controller::State::new(Arc::new(String::from("/tmp/test_git_events_runner_context")))
    }

    #[tokio::test]
    async fn readiness_responses_ready() {
        let state = get_test_controller_state().await;

        // Set ready
        {
            let mut ready = state.ready.write().await;
            *ready = true;
        }
        // Assert ready
        let (code, msg) = handle_ready(State(state.clone())).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(msg, "Ready");
    }

    #[tokio::test]
    async fn readiness_responses_not_ready() {
        let state = get_test_controller_state().await;

        // Assert not ready
        let (code, msg) = handle_ready(State(state.clone())).await;
        assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(msg, "Not ready");
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn nonexistent_post_trigger_webhook() {
        let state = init().await;

        let res = handle_post_trigger_webhook(
            State(state),
            Path(("nonexistent-namespace".into(), "nonexistent-trigger-name".into())),
            HeaderMap::new(),
        )
        .await;
        assert!(res.is_err());
        assert_eq!(res.err().unwrap(), WebError::TriggerNotFound);
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn nonexistent_post_source_webhook() {
        let state = init().await;

        let res = handle_post_source_webhook(
            State(state),
            Path((
                "nonexistent-namespace".into(),
                "nonexistent-trigger-name".into(),
                "nonexistent-source-name".into(),
            )),
            HeaderMap::new(),
        )
        .await;
        assert!(res.is_err());
        assert_eq!(res.err().unwrap(), WebError::TriggerNotFound);
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn good_multi_source_trigger_webhook_all_sources() {
        let state = init().await;

        let trigger_name = "multi-source-trigger-all-sources";
        let source_name = "source-name";

        let api = Api::<WebhookTrigger>::namespaced(state.client.clone(), NAMESPACE);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create good multi-source webhook trigger
        let trigger = WebhookTrigger::test_anonymous_webhook(trigger_name, NAMESPACE, vec![source_name.into()], true);
        let _trigger = api.create(&pp, &trigger).await.unwrap();

        // Test all sources request
        let res = handle_post_trigger_webhook(
            State(state.clone()),
            Path((NAMESPACE.into(), trigger_name.into())),
            HeaderMap::new(),
        )
        .await;
        assert!(res.is_ok());
        let res = res.unwrap();
        assert_eq!(res.status, WebResponseStatus::Ok);
        assert_eq!(res.message, "job has been scheduled");
        assert!(res.task_id.is_some());

        // Clean up
        let _result = api.delete(trigger_name, &dp).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn good_single_source_trigger_webhook_with_all_sources_request() {
        let state = init().await;

        let trigger_name = "single-source-trigger-all-sources-request";
        let source_name = "source-name";

        let api = Api::<WebhookTrigger>::namespaced(state.client.clone(), NAMESPACE);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create good multi-source webhook trigger
        let trigger = WebhookTrigger::test_anonymous_webhook(trigger_name, NAMESPACE, vec![source_name.into()], false);
        let _trigger = api.create(&pp, &trigger).await.unwrap();

        // Test all sources request
        let res = handle_post_trigger_webhook(
            State(state.clone()),
            Path((NAMESPACE.into(), trigger_name.into())),
            HeaderMap::new(),
        )
        .await;
        assert!(res.is_err());
        assert_eq!(res.err().unwrap(), WebError::ForbiddenMultiSource);

        // Clean up
        let _result = api.delete(trigger_name, &dp).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn good_multi_source_trigger_webhook_single_source() {
        let state = init().await;

        let trigger_name = "multi-source-trigger-single-source";
        let source_name = "source-name";

        let api = Api::<WebhookTrigger>::namespaced(state.client.clone(), NAMESPACE);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create good multi-source webhook trigger
        let trigger = WebhookTrigger::test_anonymous_webhook(trigger_name, NAMESPACE, vec![source_name.into()], true);
        let _trigger = api.create(&pp, &trigger).await.unwrap();

        // Test single sources request
        let res = handle_post_source_webhook(
            State(state.clone()),
            Path((NAMESPACE.into(), trigger_name.into(), source_name.into())),
            HeaderMap::new(),
        )
        .await;
        assert!(res.is_ok());
        let res = res.unwrap();
        assert_eq!(res.status, WebResponseStatus::Ok);
        assert_eq!(res.message, "job has been scheduled");
        assert!(res.task_id.is_some());

        // Clean up
        let _result = api.delete(trigger_name, &dp).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn good_multi_source_trigger_webhook_wrong_single_source() {
        let state = init().await;

        let trigger_name = "multi-source-trigger-wrong-single-source";
        let source_name = "source-name";

        let api = Api::<WebhookTrigger>::namespaced(state.client.clone(), NAMESPACE);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create good multi-source webhook trigger
        let trigger = WebhookTrigger::test_anonymous_webhook(trigger_name, NAMESPACE, vec![source_name.into()], true);
        let _trigger = api.create(&pp, &trigger).await.unwrap();

        // Test single sources request with wrong source
        let res = handle_post_source_webhook(
            State(state.clone()),
            Path((NAMESPACE.into(), trigger_name.into(), "wrong-source-name".into())),
            HeaderMap::new(),
        )
        .await;
        assert!(res.is_err());
        assert_eq!(res.err().unwrap(), WebError::SourceNotFound);

        // Clean up
        let _result = api.delete(trigger_name, &dp).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn good_multi_source_trigger_webhook_with_good_auth() {
        let state = init().await;

        let trigger_name = "multi-source-trigger-with-good-auth";
        let source_name = "source-name";

        let api = Api::<WebhookTrigger>::namespaced(state.client.clone(), NAMESPACE);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create good multi-source webhook trigger
        let trigger =
            WebhookTrigger::test_secure_webhook(trigger_name, NAMESPACE, vec![source_name.into()], true, SECRET_NAME);
        let _trigger = api.create(&pp, &trigger).await.unwrap();

        // Test all sources request
        let mut headers = HeaderMap::new();
        headers.append("x-auth-token", HeaderValue::from_static(SECRET_TOKEN));

        let res = handle_post_trigger_webhook(
            State(state.clone()),
            Path((NAMESPACE.into(), trigger_name.into())),
            headers,
        )
        .await;

        assert!(res.is_ok());
        let res = res.unwrap();
        assert_eq!(res.status, WebResponseStatus::Ok);
        assert_eq!(res.message, "job has been scheduled");
        assert!(res.task_id.is_some());

        // Clean up
        let _result = api.delete(trigger_name, &dp).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn good_multi_source_trigger_webhook_with_wrong_auth() {
        let state = init().await;

        let trigger_name = "multi-source-trigger-with-wrong-auth";
        let source_name = "source-name";

        let api = Api::<WebhookTrigger>::namespaced(state.client.clone(), NAMESPACE);
        let pp = PostParams::default();
        let dp = DeleteParams::default();

        // Create good multi-source webhook trigger
        let trigger =
            WebhookTrigger::test_secure_webhook(trigger_name, NAMESPACE, vec![source_name.into()], true, SECRET_NAME);
        let _trigger = api.create(&pp, &trigger).await.unwrap();

        // Test all sources request with wrong header
        let mut headers = HeaderMap::new();
        headers.append("xxx-auth-token", HeaderValue::from_static(SECRET_TOKEN));

        let res = handle_post_trigger_webhook(
            State(state.clone()),
            Path((NAMESPACE.into(), trigger_name.into())),
            headers,
        )
        .await;

        assert!(res.is_err());
        assert_eq!(res.err().unwrap(), WebError::Unauthorized);

        // Test all sources request with wrong token
        let mut headers = HeaderMap::new();
        headers.append("x-auth-token", HeaderValue::from_static("something definitely wrong"));

        let res = handle_post_trigger_webhook(
            State(state.clone()),
            Path((NAMESPACE.into(), trigger_name.into())),
            headers,
        )
        .await;

        assert!(res.is_err());
        assert_eq!(res.err().unwrap(), WebError::Forbidden);

        // Clean up
        let _result = api.delete(trigger_name, &dp).await.unwrap();
    }
}
