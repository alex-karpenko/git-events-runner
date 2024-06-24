use crate::cli::CLI_CONFIG;
use crate::config::RuntimeConfig;
use crate::resources::action::{
    ACTION_JOB_ACTION_KIND_LABEL, ACTION_JOB_ACTION_NAME_LABEL, ACTION_JOB_SOURCE_KIND_LABEL,
    ACTION_JOB_SOURCE_NAME_LABEL, ACTION_JOB_TRIGGER_KIND_LABEL, ACTION_JOB_TRIGGER_NAME_LABEL,
};
use crate::{Error, Result};
use futures::{future::ready, StreamExt};
use k8s_openapi::api::batch::v1::JobStatus;
use k8s_openapi::{api::batch::v1::Job, Metadata};
use kube::api::{ObjectMeta, PostParams};
use kube::runtime::reflector::ObjectRef;
use kube::ResourceExt;
use kube::{
    runtime::{reflector, watcher, WatchStreamExt},
    Api, Client,
};
use lazy_static::lazy_static;
use prometheus::{histogram_opts, opts, register, Histogram, HistogramVec, IntCounterVec, IntGauge};
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, SystemTime};
use std::{
    fmt::Debug,
    sync::{Arc, OnceLock},
};
use strum::Display;
use tokio::sync::{mpsc, watch, RwLock};
use tracing::{debug, error, info, trace, warn};

const ACTION_JOB_IDENTITY_LABEL: &str = "git-events-runner.rs/controller-identity";

static JOBS_QUEUE: OnceLock<Arc<JobsQueue>> = OnceLock::new();

lazy_static! {
    static ref METRICS: Metrics = Metrics::default().register();
}

#[derive(Clone, Debug, PartialEq, Display)]
enum JobCompletionStatus {
    Succeed,
    Failed,
    Expired,
    Deleted,
    CreateError,
}

struct Metrics {
    current_limit: IntGauge,
    running_jobs: IntGauge,
    waiting_jobs: IntGauge,
    waiting_duration: Histogram,
    completed_count: IntCounterVec,
    completed_duration: HistogramVec,
}

impl Default for Metrics {
    fn default() -> Self {
        let cli_config = CLI_CONFIG.get().unwrap();

        let current_limit = IntGauge::new(
            format!("{}_jobs_queue_limit", cli_config.metrics_prefix),
            "The limit of simultaneously running jobs",
        )
        .unwrap();

        let running_jobs = IntGauge::new(
            format!("{}_jobs_queue_running_jobs", cli_config.metrics_prefix),
            "The current number of waiting in the queue jobs",
        )
        .unwrap();

        let waiting_jobs = IntGauge::new(
            format!("{}_jobs_queue_waiting_jobs", cli_config.metrics_prefix),
            "The current number of runnings jobs",
        )
        .unwrap();

        let waiting_duration = Histogram::with_opts(
            histogram_opts!(
                format!("{}_jobs_queue_waiting_duration_seconds", cli_config.metrics_prefix),
                "The jobs queue waiting time in seconds"
            )
            .buckets(vec![0.1, 0.5, 1., 2., 5., 10., 20., 60.]),
        )
        .unwrap();

        let completed_count = IntCounterVec::new(
            opts!(
                format!("{}_jobs_queue_completed_count", cli_config.metrics_prefix),
                "The number completed jobs",
            ),
            &[
                "namespace",
                "trigger_kind",
                "trigger_name",
                "source_kind",
                "source_name",
                "action_kind",
                "action_name",
                "status",
            ],
        )
        .unwrap();

        let completed_duration = HistogramVec::new(
            histogram_opts!(
                format!("{}_jobs_queue_completed_duration_seconds", cli_config.metrics_prefix),
                "The jobs execution time in seconds"
            )
            .buckets(vec![1., 5., 10., 30., 60., 120., 300.]),
            &[
                "namespace",
                "trigger_kind",
                "trigger_name",
                "source_kind",
                "source_name",
                "action_kind",
                "action_name",
                "status",
            ],
        )
        .unwrap();

        Self {
            current_limit,
            running_jobs,
            waiting_jobs,
            waiting_duration,
            completed_count,
            completed_duration,
        }
    }
}
impl Metrics {
    fn register(self) -> Self {
        register(Box::new(self.current_limit.clone())).unwrap();
        register(Box::new(self.running_jobs.clone())).unwrap();
        register(Box::new(self.waiting_jobs.clone())).unwrap();
        register(Box::new(self.waiting_duration.clone())).unwrap();
        register(Box::new(self.completed_count.clone())).unwrap();
        register(Box::new(self.completed_duration.clone())).unwrap();

        self
    }

    fn update_gauges(&self, waiting_jobs: usize) {
        let running_jobs = JOBS_QUEUE.get().unwrap().running_jobs();

        self.running_jobs.set(running_jobs.try_into().unwrap());
        self.current_limit
            .set(RuntimeConfig::get().action.max_running_jobs.try_into().unwrap());

        self.waiting_jobs.set(waiting_jobs.try_into().unwrap());
    }

    fn count_and_measure_completed(&self, job: &Job, status: JobCompletionStatus) {
        let job = job.clone();
        let status_str = status.to_string();
        let namespace = job.metadata.namespace.unwrap();

        let job_labels = job.metadata.labels.unwrap();
        let action_kind = job_labels.get(ACTION_JOB_ACTION_KIND_LABEL).unwrap();
        let action_name = job_labels.get(ACTION_JOB_ACTION_NAME_LABEL).unwrap();
        let source_kind = job_labels.get(ACTION_JOB_SOURCE_KIND_LABEL).unwrap();
        let source_name = job_labels.get(ACTION_JOB_SOURCE_NAME_LABEL).unwrap();
        let trigger_kind = job_labels.get(ACTION_JOB_TRIGGER_KIND_LABEL).unwrap();
        let trigger_name = job_labels.get(ACTION_JOB_TRIGGER_NAME_LABEL).unwrap();

        let labels: [&str; 8] = [
            namespace.as_str(),
            trigger_kind,
            trigger_name,
            source_kind,
            source_name,
            action_kind,
            action_name,
            status_str.as_str(),
        ];

        if let Ok(metric) = self.completed_count.get_metric_with_label_values(&labels) {
            metric.inc();
        }

        match status {
            JobCompletionStatus::Expired | JobCompletionStatus::Deleted | JobCompletionStatus::CreateError => {}
            JobCompletionStatus::Succeed | JobCompletionStatus::Failed => {
                if let Ok(metric) = self.completed_duration.get_metric_with_label_values(&labels) {
                    let start = job.status.unwrap_or_default().start_time;
                    if let Some(start) = start {
                        let elapsed = SystemTime::now().duration_since(start.0.into());
                        if let Ok(elapsed) = elapsed {
                            metric.observe(elapsed.as_secs_f64());
                        }
                    }
                }
            }
        }
    }

    fn measure_waiting_duration(&self, duration: Duration) {
        self.waiting_duration.observe(duration.as_secs_f64());
    }
}

pub struct JobsQueue {
    client: Client,
    identity: String,
    queue_event_tx: watch::Sender<()>,
    queue_jobs_tx: mpsc::UnboundedSender<JobsQueueItem>,
    queue_jobs_rx: RwLock<mpsc::UnboundedReceiver<JobsQueueItem>>,
    running_jobs: AtomicUsize,
}

impl JobsQueue {
    fn running_jobs(&self) -> usize {
        self.running_jobs.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Debug for JobsQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobsQueue").field("identity", &self.identity).finish()
    }
}

struct JobsQueueItem {
    job: Job,
    enqueued_at: SystemTime,
    expire_after: Option<Duration>,
}

impl JobsQueueItem {
    fn is_expired(&self) -> bool {
        let expired_after = self.expire_after.unwrap_or(Duration::from_secs(
            RuntimeConfig::get().action.job_waiting_timeout_seconds,
        ));

        SystemTime::now() > self.enqueued_at.checked_add(expired_after).unwrap()
    }
}

impl JobsQueue {
    /// Watch and store changes of OURS K8s jobs.
    /// We identify jobs as `ours` by label (ACTION_JOB_IDENTITY_LABEL) with the value of our own identity.
    /// So each controller instance watches its own jobs only.
    ///
    /// This method is used to calculate a number of the jobs running by this particular instance,
    /// and apply limits on that number.
    pub async fn init_and_watch(client: Client, identity: String, mut shutdown: watch::Receiver<bool>) {
        let (queue_event_tx, mut queue_event_rx) = watch::channel(());
        let (queue_jobs_tx, queue_jobs_rx) = mpsc::unbounded_channel();
        let (store, writer) = reflector::store();
        let mut statuses: HashMap<ObjectRef<Job>, JobStatus> = HashMap::new();

        JOBS_QUEUE
            .set(Arc::new(Self {
                client: client.clone(),
                identity: identity.clone(),
                queue_event_tx: queue_event_tx.clone(),
                queue_jobs_tx,
                queue_jobs_rx: RwLock::new(queue_jobs_rx),
                running_jobs: AtomicUsize::new(0),
            }))
            .unwrap();

        let jobs_api: Api<Job> = Api::all(client);
        let watcher_config =
            watcher::Config::default().labels(format!("{ACTION_JOB_IDENTITY_LABEL}={}", identity).as_str());

        let jobs_stream = reflector::reflector(writer, watcher(jobs_api, watcher_config))
            .touched_objects()
            .for_each(|job| {
                debug!(len = store.len(), "update jobs store");
                if let Ok(job) = job {
                    let name = job.metadata().name.clone().unwrap();
                    let ns = job.metadata().namespace.clone().unwrap();
                    let jobs_queue = JOBS_QUEUE.get().unwrap();

                    // Filter duplicates by watching on resource status changes.
                    let key = ObjectRef::new(&name).within(&ns);
                    if let Some(store_status) = &store.get(&key).unwrap_or_default().status {
                        if let Some(prev_status) = statuses.get(&key) {
                            if store_status != prev_status {
                                trace!(status = ?store_status, "job status changed");
                                // Since we run jobs with parallelism=1 and w/o restarts,
                                // we look for just `succeed` or `failed` in the status,
                                // but not for counters under them.
                                if store_status.succeeded.is_some() {
                                    info!(namespace = %ns, job = %name, "job completed successfully");
                                    jobs_queue
                                        .running_jobs
                                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                                    METRICS.count_and_measure_completed(&job, JobCompletionStatus::Succeed);
                                } else if store_status.failed.is_some() {
                                    warn!(namespace = %ns, job = %name, "job failed");
                                    jobs_queue
                                        .running_jobs
                                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                                    METRICS.count_and_measure_completed(&job, JobCompletionStatus::Failed);
                                }
                            }
                        }

                        if statuses.insert(key, store_status.clone()).is_none() {
                            jobs_queue
                                .running_jobs
                                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        };
                    } else if let Some(status) = statuses.remove(&key) {
                        if status.succeeded.is_none() && status.failed.is_none() {
                            warn!(namespace = %ns, job = %name, "unfinished job deleted");
                            jobs_queue
                                .running_jobs
                                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                            METRICS.count_and_measure_completed(&job, JobCompletionStatus::Deleted);
                        }
                    }
                }
                let _ = queue_event_tx.send(());
                ready(())
            });

        METRICS.update_gauges(0);

        tokio::select! {
            biased;
            // Listen on shutdown events
            _ = async move {
                while shutdown.changed().await.is_ok() {
                    if *shutdown.borrow_and_update() {
                        debug!("shutdown requested");
                        break;
                    }
                }
            } => {
                debug!("shutdown event loop completed");
            }
            // Listen on jobs queue events
            _ = async move {
                while queue_event_rx.changed().await.is_ok() {
                    debug!("jobs queue updated");
                    let _ = queue_event_rx.borrow_and_update();
                    JobsQueue::run_queue().await;
                }
            } => {
                debug!("jobs queue event loop completed");
            }
            // k8s jobs stream events
            _ = jobs_stream => {
                debug!("jobs stream completed");
            },
            // Listen on config change events
            _ = async move {
                let mut config_rx = RuntimeConfig::channel();
                while config_rx.changed().await.is_ok() {
                    debug!("runtime config updated");
                    let _ = config_rx.borrow_and_update();
                    JobsQueue::run_queue().await;
                }
            } => {
                debug!("config changes event loop completed");
            }
        }
    }

    /// Checks if there are jobs in the queue to run and if there is free capacity
    /// (check store for number of running jobs is less of the limit)
    /// then starts as many jobs as possible ot satisfy limits.
    async fn run_queue() {
        let queue = JOBS_QUEUE.get().unwrap();
        let mut queue_jobs_rx = queue.queue_jobs_rx.write().await;

        let running_jobs = queue.running_jobs();
        let mut waiting_jobs = queue_jobs_rx.len();
        let max_running_jobs = RuntimeConfig::get().action.max_running_jobs;

        METRICS.update_gauges(waiting_jobs);

        if running_jobs > max_running_jobs {
            warn!(%running_jobs, "jobs queue capacity is exceeded");
            return;
        }

        let mut free_capacity = max_running_jobs - running_jobs;
        debug!(%running_jobs, %waiting_jobs, %free_capacity, "jobs queue state");

        while waiting_jobs > 0 && free_capacity > 0 {
            if let Some(job_item) = queue_jobs_rx.recv().await {
                let ns = job_item.job.namespace().unwrap();
                let name = job_item.job.name_any();

                waiting_jobs -= 1;

                if !job_item.is_expired() {
                    debug!(namespace = %ns, job = %name , "creating job");
                    METRICS.measure_waiting_duration(SystemTime::now().duration_since(job_item.enqueued_at).unwrap());

                    let jobs_api: Api<Job> = Api::namespaced(queue.client.clone(), &ns);
                    let result = jobs_api
                        .create(&PostParams::default(), &job_item.job)
                        .await
                        .map_err(Error::KubeError);

                    match result {
                        Ok(_) => {
                            free_capacity -= 1;
                            debug!(namespace = %ns, job = %name , "job created");
                        }
                        Err(error) => {
                            error!(namespace = %ns, job = %name, %error, "error creating job");
                            METRICS.count_and_measure_completed(&job_item.job, JobCompletionStatus::CreateError);
                        }
                    }
                } else {
                    warn!(namespace = %ns, job = %name, "job expired");
                    METRICS.count_and_measure_completed(&job_item.job, JobCompletionStatus::Expired);
                }

                METRICS.update_gauges(waiting_jobs);
            } else {
                debug!("jobs queue is empty");
                break;
            }
        }
    }

    /// Enqueue Jobs.
    /// Add/update necessary metadata and
    /// put job with queue timeout into the queue and trigger run_queue.
    pub async fn enqueue(job: Job, ns: &str, expire_after: Option<Duration>) -> Result<()> {
        if let Some(queue) = JOBS_QUEUE.get() {
            debug!(namespace = %ns, job = %job.name_any(), "enqueue job");

            // Add identity label to track job
            let mut labels = job.metadata.labels.unwrap_or_default();
            labels.insert(ACTION_JOB_IDENTITY_LABEL.into(), queue.identity.clone());

            // Update labels and namespace
            let job = Job {
                metadata: ObjectMeta {
                    namespace: Some(ns.to_string()),
                    labels: Some(labels),
                    ..job.metadata
                },
                ..job
            };

            // Put the job into the queue
            let job_item = JobsQueueItem {
                job,
                enqueued_at: SystemTime::now(),
                expire_after,
            };
            let jobs_tx = queue.queue_jobs_tx.clone();
            jobs_tx
                .send(job_item)
                .map_err(|e| Error::JobsQueueError(e.to_string()))?;

            // Trigger run_queue method indirectly.
            let event_tx = queue.queue_event_tx.clone();
            event_tx.send(()).map_err(|e| Error::JobsQueueError(e.to_string()))?;

            Ok(())
        } else {
            Err(Error::JobsQueueError("jobs queue isn't initialized".into()))
        }
    }
}
