use crate::config::RuntimeConfig;
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
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::time::{Duration, SystemTime};
use std::{
    fmt::Debug,
    sync::{Arc, OnceLock},
};
use tokio::sync::{mpsc, watch, RwLock};
use tracing::{debug, error, info, trace, warn};

const ACTION_JOB_IDENTITY_LABEL: &str = "git-events-runner.rs/controller-identity";

static JOBS_QUEUE: OnceLock<Arc<JobsQueue>> = OnceLock::new();

pub struct JobsQueue {
    client: Client,
    identity: String,
    queue_event_tx: watch::Sender<()>,
    queue_jobs_tx: mpsc::UnboundedSender<(Job, SystemTime)>,
    queue_jobs_rx: RwLock<mpsc::UnboundedReceiver<(Job, SystemTime)>>,
    running_jobs: AtomicUsize,
}

impl Debug for JobsQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobsQueue").field("identity", &self.identity).finish()
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
                                } else if store_status.failed.is_some() {
                                    warn!(namespace = %ns, job = %name, "job failed");
                                    jobs_queue
                                        .running_jobs
                                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
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
                        }
                    }
                }
                let _ = queue_event_tx.send(());
                ready(())
            });

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

        let running_jobs = queue.running_jobs.load(std::sync::atomic::Ordering::SeqCst);
        if running_jobs > RuntimeConfig::get().action.max_running_jobs {
            warn!(%running_jobs, "jobs queue capacity is exceeded");
            return;
        }

        let mut free_capacity = RuntimeConfig::get().action.max_running_jobs - running_jobs;
        let mut waiting_jobs = queue_jobs_rx.len();
        let now = SystemTime::now();

        debug!(%running_jobs, %waiting_jobs, %free_capacity, "jobs queue state");

        while waiting_jobs > 0 && free_capacity > 0 {
            if let Some((job, expiration_time)) = queue_jobs_rx.recv().await {
                let ns = job.namespace().unwrap();
                let name = job.name_any();
                if now < expiration_time {
                    debug!(namespace = %ns, job = %name , "creating job");
                    let jobs_api: Api<Job> = Api::namespaced(queue.client.clone(), &ns);
                    let result = jobs_api
                        .create(&PostParams::default(), &job)
                        .await
                        .map_err(Error::KubeError);

                    match result {
                        Ok(_) => {
                            waiting_jobs -= 1;
                            free_capacity -= 1;
                            debug!(namespace = %ns, job = %name , "job created");
                        }
                        Err(error) => {
                            error!(namespace = %ns, job = %name, %error, "error creating job")
                        }
                    }
                } else {
                    warn!(namespace = %ns, job = %name, "job expired");
                }
            } else {
                debug!("jobs queue is empty");
                break;
            }
        }
    }

    /// Enqueue Jobs.
    /// Add/update necessary metadata and
    /// put job with queue timeout into the queue and trigger run_queue.
    pub async fn enqueue(job: Job, ns: &str, timeout: Option<Duration>) -> Result<Job> {
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

            let timeout = timeout.unwrap_or(Duration::from_secs(
                RuntimeConfig::get().action.job_waiting_timeout_seconds,
            ));
            let expiration_time = SystemTime::now().checked_add(timeout).unwrap();

            // Put the job into the queue
            let jobs_tx = queue.queue_jobs_tx.clone();
            jobs_tx
                .send((job.clone(), expiration_time))
                .map_err(|e| Error::JobsQueueError(e.to_string()))?;

            // Trigger run_queue method indirectly.
            let event_tx = queue.queue_event_tx.clone();
            event_tx.send(()).map_err(|e| Error::JobsQueueError(e.to_string()))?;

            Ok(job)
        } else {
            Err(Error::JobsQueueError("jobs queue isn't initialized".into()))
        }
    }
}
