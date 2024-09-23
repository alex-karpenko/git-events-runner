//! Code related to web-related stuff, handles calls to:
//! - WebhookTrigger trigger
//! - liveness and readiness probes
//! - metrics
use axum::http::Request;
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
use prometheus::{histogram_opts, opts, register, Encoder, HistogramVec, IntCounterVec, TextEncoder};
use sacs::{
    scheduler::{Scheduler, TaskScheduler},
    task::TaskId,
};
use serde::Serialize;
use std::fmt::Display;
use std::sync::LazyLock;
use std::{sync::Arc, time::Duration};
use strum::Display;
use tokio::time::Instant;
use tokio::{
    net::TcpListener,
    sync::{watch, RwLock},
};
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::key_extractor::KeyExtractor;
use tower_governor::{GovernorError, GovernorLayer};
use tracing::{debug, error, info, instrument, trace, warn};
use uuid::Uuid;

use crate::cache::{ApiCache, SecretsCache};
use crate::cli::CliConfig;
use crate::config::RuntimeConfig;
use crate::controller::State as AppState;
use crate::resources::trigger::{Trigger, TriggerTaskSources, WebhookTrigger, WebhookTriggerSpec};
use crate::{get_trace_id, Error};

static METRICS: LazyLock<Metrics> = LazyLock::new(|| Metrics::default().register());

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
#[derive(Clone, FromRequest, Serialize, Debug)]
#[from_request(via(axum::Json), rejection(WebError))]
struct WebResponseJson {
    status: WebResponseStatus,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
}

#[derive(Clone, Serialize, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
enum WebResponseStatus {
    Ok,
    Error,
}

#[derive(Clone, Display, Debug, PartialEq)]
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

/// Represents parameter for requests rate limiter
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RequestsRateLimitParams {
    /// Size of the burst bucket
    burst: u32,
    /// Period to replenish single bucket item
    period: Duration,
}

/// Represents a set of request rate limits parameters to apply to the hooks-web
#[derive(Debug)]
pub struct RequestRateLimitsConfig {
    /// Global RRL
    pub global: Option<RequestsRateLimitParams>,
    /// Per trigger RRL
    pub trigger: Option<RequestsRateLimitParams>,
    /// Per triggers' source rrl
    pub source: Option<RequestsRateLimitParams>,
}

impl TryFrom<&str> for RequestsRateLimitParams {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let splitted: Vec<String> = value.split("/").map(String::from).collect();
        if splitted.len() != 2 {
            return Err(Error::InvalidRequestRateLimit(value.into()));
        }
        let burst = splitted[0]
            .parse::<u32>()
            .map_err(|_| Error::InvalidRequestRateLimit(value.into()))?;
        let period = splitted[1]
            .parse::<u64>()
            .map_err(|_| Error::InvalidRequestRateLimit(value.into()))?;

        if burst == 0 || period == 0 {
            return Err(Error::InvalidRequestRateLimit(value.into()));
        }

        Ok(Self {
            burst,
            period: Duration::from_secs_f64(period as f64 / burst as f64),
        })
    }
}

impl RequestRateLimitsConfig {
    fn add_rrl_layer(&self, router: Router) -> Router {
        let router = if let Some(rrl) = &self.global {
            RequestsRateLimiter::Global.add_rrl_layer(rrl.burst, rrl.period, router)
        } else {
            router
        };

        let router = if let Some(rrl) = &self.trigger {
            RequestsRateLimiter::Trigger.add_rrl_layer(rrl.burst, rrl.period, router)
        } else {
            router
        };

        if let Some(rrl) = &self.source {
            RequestsRateLimiter::Source.add_rrl_layer(rrl.burst, rrl.period, router)
        } else {
            router
        }
    }
}

#[derive(Debug, Clone)]
enum RequestsRateLimiter {
    Global,
    Trigger,
    Source,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
struct RateLimiterKey {
    ns: Option<String>,
    trigger: Option<String>,
    source: Option<String>,
    strip_source: bool,
}

impl Display for RateLimiterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut key = String::new();
        if let Some(ns) = &self.ns {
            key.push_str(ns);
            key.push('/');
        }
        if let Some(trigger) = &self.trigger {
            key.push_str(trigger);
            key.push('/');
        }
        if !self.strip_source {
            if let Some(source) = &self.source {
                key.push_str(source);
            }
        }
        write!(f, "{}", key)
    }
}

impl KeyExtractor for RequestsRateLimiter {
    type Key = RateLimiterKey;

    fn name(&self) -> &'static str {
        match self {
            RequestsRateLimiter::Global => "global",
            RequestsRateLimiter::Trigger => "trigger",
            RequestsRateLimiter::Source => "trigger source",
        }
    }

    fn extract<T>(&self, req: &Request<T>) -> Result<Self::Key, GovernorError> {
        self.extract_path_key(req.uri().path())
    }

    fn key_name(&self, key: &Self::Key) -> Option<String> {
        match self {
            RequestsRateLimiter::Global => None,
            RequestsRateLimiter::Trigger | RequestsRateLimiter::Source => Some(key.to_string()),
        }
    }
}

impl RequestsRateLimiter {
    #[allow(clippy::result_large_err)]
    fn extract_path_key(&self, path: &str) -> Result<RateLimiterKey, GovernorError> {
        match self {
            RequestsRateLimiter::Global => Ok(RateLimiterKey::default()),
            RequestsRateLimiter::Trigger => {
                let splitted_path: Vec<&str> = path.split("/").collect();
                if splitted_path.len() < 3 || splitted_path[1].is_empty() || splitted_path[2].is_empty() {
                    return Err(GovernorError::UnableToExtractKey);
                }
                Ok(RateLimiterKey {
                    ns: Some(String::from(splitted_path[1])),
                    trigger: Some(String::from(splitted_path[2])),
                    ..Default::default()
                })
            }
            RequestsRateLimiter::Source => {
                let splitted_path: Vec<&str> = path.split("/").collect();
                if splitted_path.len() >= 4
                    && !(splitted_path[1].is_empty() || splitted_path[2].is_empty() || splitted_path[3].is_empty())
                {
                    Ok(RateLimiterKey {
                        ns: Some(String::from(splitted_path[1])),
                        trigger: Some(String::from(splitted_path[2])),
                        source: Some(String::from(splitted_path[3])),
                        strip_source: false,
                    })
                } else {
                    if splitted_path.len() < 3 || splitted_path[1].is_empty() || splitted_path[2].is_empty() {
                        return Err(GovernorError::UnableToExtractKey);
                    }
                    Ok(RateLimiterKey {
                        ns: Some(String::from(splitted_path[1])),
                        trigger: Some(String::from(splitted_path[2])),
                        source: Some(String::from(Uuid::new_v4())),
                        strip_source: true,
                    })
                }
            }
        }
    }
}

impl RequestsRateLimiter {
    fn add_rrl_layer(&self, burst: u32, period: Duration, router: Router) -> Router {
        let self_type = match self {
            RequestsRateLimiter::Global => RequestsRateLimiter::Global,
            RequestsRateLimiter::Trigger => RequestsRateLimiter::Trigger,
            RequestsRateLimiter::Source => RequestsRateLimiter::Source,
        };

        let config = Arc::new(
            GovernorConfigBuilder::default()
                .key_extractor(self_type)
                .burst_size(burst)
                .period(period)
                .finish()
                .unwrap(),
        );

        router.layer(GovernorLayer { config })
    }
}

struct Metrics {
    duration: HistogramVec,
    count: IntCounterVec,
}

impl Metrics {
    fn register(self) -> Self {
        register(Box::new(self.duration.clone())).unwrap();
        register(Box::new(self.count.clone())).unwrap();
        self
    }

    fn count_and_measure(
        &self,
        namespace: &str,
        trigger: &str,
        start: &Instant,
        result: &Result<WebResponseJson, WebError>,
    ) {
        let latency = start.elapsed().as_secs_f64();
        let status = result.clone().into_response().status();
        let labels: [&str; 3] = [namespace, trigger, status.as_str()];

        METRICS
            .duration
            .get_metric_with_label_values(&labels)
            .unwrap()
            .observe(latency);
        METRICS.count.get_metric_with_label_values(&labels).unwrap().inc();
    }
}

impl Default for Metrics {
    fn default() -> Self {
        let cli_config = CliConfig::get();

        let duration = HistogramVec::new(
            histogram_opts!(
                format!("{}_webhook_requests_duration_seconds", cli_config.metrics_prefix),
                "The time of webhook request scheduling in seconds"
            )
            .buckets(vec![0.001, 0.01, 0.1, 0.5, 1.]),
            &["namespace", "trigger_name", "status"],
        )
        .unwrap();

        let count = IntCounterVec::new(
            opts!(
                format!("{}_webhook_requests_count", cli_config.metrics_prefix),
                "The number of webhook requests",
            ),
            &["namespace", "trigger_name", "status"],
        )
        .unwrap();

        Self { duration, count }
    }
}

/// Returns web server Future with endpoints for health/live checks and metrics responder.
/// It exits by receiving anything from `shutdown` channel.
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
    request_rate_limits: RequestRateLimitsConfig,
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
    let app = request_rate_limits.add_rrl_layer(app);

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

#[instrument("ready", level = "trace", skip_all, fields(trace_id = %get_trace_id()))]
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

#[instrument("metrics", level = "trace", skip_all, fields(trace_id = %get_trace_id()))]
async fn handle_metrics(State(_state): State<AppState>) -> (StatusCode, String) {
    trace!("metrics requested");

    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    let metric_families = prometheus::gather();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    let output = String::from_utf8(buffer.clone()).unwrap();

    (StatusCode::OK, output)
}

/// Trigger jobs for all sources of the trigger
#[instrument("trigger webhook", skip_all, fields(namespace, trigger, trace_id = %get_trace_id()))]
async fn handle_post_trigger_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<WebResponseJson, WebError> {
    let start = Instant::now();

    let result = async {
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
    .await;

    METRICS.count_and_measure(&namespace, &trigger, &start, &result);
    result
}

/// Single source trigger run
#[instrument("source webhook", skip_all, fields(namespace, trigger, source, trace_id = %get_trace_id()))]
async fn handle_post_source_webhook(
    State(state): State<WebState>,
    Path((namespace, trigger, source)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<WebResponseJson, WebError> {
    let start = Instant::now();

    let result = async {
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
                // by specifying `single source`
                // we restrict the scope of the task.
                TriggerTaskSources::Single(source.clone()),
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
    .await;

    METRICS.count_and_measure(&namespace, &trigger, &start, &result);
    result
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

    use axum::extract::State;
    use axum::http::HeaderValue;
    use k8s_openapi::api::core::v1::{Namespace, Secret};
    use kube::api::{DeleteParams, PostParams};
    use kube::{Api, Resource};
    use tokio::sync::OnceCell;

    use crate::controller;

    use super::*;

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

        // Test single sources request with a wrong source
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

        // Test all sources request with a wrong header
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

    #[test]
    fn requests_rate_limit_valid() {
        assert_eq!(
            RequestsRateLimitParams::try_from("1/1").unwrap(),
            RequestsRateLimitParams {
                burst: 1,
                period: Duration::from_secs(1)
            }
        );

        assert_eq!(
            RequestsRateLimitParams::try_from("1/10").unwrap(),
            RequestsRateLimitParams {
                burst: 1,
                period: Duration::from_secs(10)
            }
        );

        assert_eq!(
            RequestsRateLimitParams::try_from("5/1").unwrap(),
            RequestsRateLimitParams {
                burst: 5,
                period: Duration::from_millis(200)
            }
        );
    }

    #[test]
    fn requests_rate_limit_invalid() {
        let tests = ["", "/", "1", "1/", "/1", "1/0", "0/1", "0/0", "q", "q/w"];

        for t in tests {
            assert!(
                matches!(
                    RequestsRateLimitParams::try_from(t),
                    Err(Error::InvalidRequestRateLimit(_))
                ),
                "Unexpected result for string '{t}'"
            );
        }
    }

    #[test]
    fn rate_limit_key_to_string() {
        assert_eq!(RateLimiterKey::default().to_string(), "");
        assert_eq!(
            RateLimiterKey {
                ns: Some("test-ns".into()),
                ..Default::default()
            }
            .to_string(),
            "test-ns/"
        );
        assert_eq!(
            RateLimiterKey {
                ns: Some("test-ns".into()),
                trigger: Some("test-trigger".into()),
                ..Default::default()
            }
            .to_string(),
            "test-ns/test-trigger/"
        );
        assert_eq!(
            RateLimiterKey {
                ns: Some("test-ns".into()),
                trigger: Some("test-trigger".into()),
                strip_source: true,
                ..Default::default()
            }
            .to_string(),
            "test-ns/test-trigger/"
        );
        assert_eq!(
            RateLimiterKey {
                ns: Some("test-ns".into()),
                trigger: Some("test-trigger".into()),
                source: Some("test-source".into()),
                strip_source: false,
            }
            .to_string(),
            "test-ns/test-trigger/test-source"
        );
        assert_eq!(
            RateLimiterKey {
                ns: Some("test-ns".into()),
                trigger: Some("test-trigger".into()),
                source: Some("test-source".into()),
                strip_source: true,
            }
            .to_string(),
            "test-ns/test-trigger/"
        );
    }

    #[test]
    fn ensure_default_rate_limiter_key_is_all_nones() {
        assert_eq!(
            RateLimiterKey::default(),
            RateLimiterKey {
                ns: None,
                trigger: None,
                source: None,
                strip_source: false
            }
        );
    }

    #[test]
    fn rate_limiter_path_extractor_global() {
        let rrl = RequestsRateLimiter::Global;
        assert_eq!(rrl.extract_path_key("/").unwrap(), RateLimiterKey::default());
        assert_eq!(rrl.extract_path_key("").unwrap(), RateLimiterKey::default());
        assert_eq!(rrl.extract_path_key("/a").unwrap(), RateLimiterKey::default());
        assert_eq!(rrl.extract_path_key("/a/").unwrap(), RateLimiterKey::default());
        assert_eq!(rrl.extract_path_key("/a/b").unwrap(), RateLimiterKey::default());
        assert_eq!(rrl.extract_path_key("/a/b/").unwrap(), RateLimiterKey::default());
        assert_eq!(rrl.extract_path_key("/a/b/c").unwrap(), RateLimiterKey::default());
        assert_eq!(rrl.extract_path_key("/a/b/c/").unwrap(), RateLimiterKey::default());
    }

    #[test]
    fn rate_limiter_path_extractor_trigger() {
        let rrl = RequestsRateLimiter::Trigger;
        assert!(matches!(
            rrl.extract_path_key(""),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("/"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("//"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("///"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("////"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("/a"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("/a/"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert_eq!(
            rrl.extract_path_key("/a/b").unwrap(),
            RateLimiterKey {
                ns: Some("a".into()),
                trigger: Some("b".into()),
                source: None,
                strip_source: false
            }
        );
        assert_eq!(
            rrl.extract_path_key("/a/b").unwrap(),
            RateLimiterKey {
                ns: Some("a".into()),
                trigger: Some("b".into()),
                source: None,
                strip_source: false
            }
        );
        assert_eq!(
            rrl.extract_path_key("/a/b/").unwrap(),
            RateLimiterKey {
                ns: Some("a".into()),
                trigger: Some("b".into()),
                source: None,
                strip_source: false
            }
        );
        assert_eq!(
            rrl.extract_path_key("/a/b/c").unwrap(),
            RateLimiterKey {
                ns: Some("a".into()),
                trigger: Some("b".into()),
                source: None,
                strip_source: false
            }
        );
        assert_eq!(
            rrl.extract_path_key("/a/b/c/").unwrap(),
            RateLimiterKey {
                ns: Some("a".into()),
                trigger: Some("b".into()),
                source: None,
                strip_source: false
            }
        );
    }

    #[test]
    fn rate_limiter_path_extractor_source() {
        let rrl = RequestsRateLimiter::Source;
        assert!(matches!(
            rrl.extract_path_key(""),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("/"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("//"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("///"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("////"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("/a"),
            Err(GovernorError::UnableToExtractKey)
        ));
        assert!(matches!(
            rrl.extract_path_key("/a/"),
            Err(GovernorError::UnableToExtractKey)
        ));

        let key = rrl.extract_path_key("/a/b").unwrap();
        assert_eq!(key.ns, Some("a".into()));
        assert_eq!(key.trigger, Some("b".into()));
        assert!(key.source.is_some());
        assert!(key.strip_source);

        let key = rrl.extract_path_key("/a/b/").unwrap();
        assert_eq!(key.ns, Some("a".into()));
        assert_eq!(key.trigger, Some("b".into()));
        assert!(key.source.is_some());
        assert!(key.strip_source);

        assert_eq!(
            rrl.extract_path_key("/a/b/c").unwrap(),
            RateLimiterKey {
                ns: Some("a".into()),
                trigger: Some("b".into()),
                source: Some("c".into()),
                strip_source: false
            }
        );
        assert_eq!(
            rrl.extract_path_key("/a/b/c/").unwrap(),
            RateLimiterKey {
                ns: Some("a".into()),
                trigger: Some("b".into()),
                source: Some("c".into()),
                strip_source: false
            }
        );
        assert_eq!(
            rrl.extract_path_key("/a/b/c//").unwrap(),
            RateLimiterKey {
                ns: Some("a".into()),
                trigger: Some("b".into()),
                source: Some("c".into()),
                strip_source: false
            }
        );

        assert!(matches!(
            rrl.extract_path_key("/a//b/c"),
            Err(GovernorError::UnableToExtractKey)
        ));
    }
}
