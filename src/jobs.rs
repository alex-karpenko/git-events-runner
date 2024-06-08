use crate::config::RuntimeConfig;
use crate::{Error, Result};
use futures::{future::ready, StreamExt};
use k8s_openapi::{api::batch::v1::Job, Metadata};
use kube::api::{ObjectMeta, PostParams};
use kube::runtime::reflector::Store;
use kube::ResourceExt;
use kube::{
    runtime::{reflector, watcher, WatchStreamExt},
    Api, Client,
};
use std::time::{Duration, SystemTime};
use std::{
    fmt::Debug,
    sync::{Arc, OnceLock},
};
use tokio::sync::{self, RwLock};
use tracing::{debug, error, info, warn};

const ACTION_JOB_IDENTITY_LABEL: &str = "git-events-runner.rs/controller-identity";

static JOBS_QUEUE: OnceLock<Arc<JobsQueue>> = OnceLock::new();

pub struct JobsQueue {
    client: Client,
    identity: String,
    store: Store<Job>,
    queue_event_tx: sync::watch::Sender<()>,
    queue_jobs_tx: sync::mpsc::UnboundedSender<(Job, SystemTime)>,
    queue_jobs_rx: RwLock<sync::mpsc::UnboundedReceiver<(Job, SystemTime)>>,
}

impl Debug for JobsQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobsQueue").field("identity", &self.identity).finish()
    }
}

impl JobsQueue {
    /// Watch and store changes of OURS K8s jobs.
    /// We identify jobs as `ours` by label (ACTION_JOB_IDENTITY_LABEL) with value of our own identity.
    /// So each controller instance watches its own jobs only.
    ///
    /// This method is used to calculate number of the jobs running by this particular instance,
    /// and apply limits on that number.
    pub async fn init_and_watch(client: Client, identity: String) {
        let (queue_event_tx, mut queue_event_rx) = sync::watch::channel(());
        let (queue_jobs_tx, queue_jobs_rx) = sync::mpsc::unbounded_channel();
        let (store, writer) = reflector::store();

        JOBS_QUEUE
            .set(Arc::new(Self {
                client: client.clone(),
                identity: identity.clone(),
                store: store.clone(),
                queue_event_tx: queue_event_tx.clone(),
                queue_jobs_tx,
                queue_jobs_rx: RwLock::new(queue_jobs_rx),
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
                    let status = job.status.unwrap();

                    debug!(?status, "job status changed");
                    // Since we run jobs with parallelism=1 and w/o restarts,
                    // we look for just `succeed` or `failed` in the status,
                    // but not for counters under them
                    if status.succeeded.is_some() {
                        info!(%ns, %name, "job completed successfully");
                    } else if status.failed.is_some() {
                        warn!(%ns, %name, "job failed");
                    }
                }
                let _ = queue_event_tx.send(());
                ready(())
            });

        tokio::select! {
            _ = jobs_stream => {
                debug!("jobs stream completed");
            },
            _ = async move {
                while queue_event_rx.changed().await.is_ok() {
                    debug!("jobs queue updated");
                    let _ = queue_event_rx.borrow_and_update();
                    JobsQueue::run_queue().await;
                }
            } => {
                debug!("jobs queue event loop completed");
            }
        }
    }

    async fn run_queue() {
        let queue = JOBS_QUEUE.get().unwrap();
        let mut queue_jobs_rx = queue.queue_jobs_rx.write().await;

        let running_jobs = queue.store.len();
        let mut waiting_jobs = queue_jobs_rx.len();
        let mut free_capacity = RuntimeConfig::get().action.max_running_jobs - running_jobs;
        let now = SystemTime::now();

        debug!(%running_jobs, %waiting_jobs, %free_capacity, "jobs queue state");

        while waiting_jobs > 0 && free_capacity > 0 {
            if let Some((job, expiration_time)) = queue_jobs_rx.recv().await {
                let ns = job.namespace().unwrap();
                let name = job.name_any();
                if now < expiration_time {
                    debug!(%ns, %name , "creating job");
                    let jobs_api: Api<Job> = Api::namespaced(queue.client.clone(), &ns);
                    let result = jobs_api
                        .create(&PostParams::default(), &job)
                        .await
                        .map_err(Error::KubeError);

                    match result {
                        Ok(_) => {
                            waiting_jobs -= 1;
                            free_capacity -= 1;
                            debug!(%ns, %name , "job created");
                        }
                        Err(error) => {
                            error!(%ns, %name, %error, "error creating job")
                        }
                    }
                } else {
                    warn!(%ns, %name, "job expired");
                }
            } else {
                debug!("jobs queue is empty");
                break;
            }
        }
    }

    /// Enqueue Jobs.
    pub async fn enqueue(job: Job, ns: &str, timeout: Option<Duration>) -> Result<Job> {
        if let Some(queue) = JOBS_QUEUE.get() {
            debug!(%ns, name = %job.name_any(), "enqueue job");

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

            let jobs_tx = queue.queue_jobs_tx.clone();
            jobs_tx
                .send((job.clone(), expiration_time))
                .map_err(|e| Error::JobsQueueError(e.to_string()))?;

            let event_tx = queue.queue_event_tx.clone();
            event_tx.send(()).map_err(|e| Error::JobsQueueError(e.to_string()))?;

            Ok(job)
        } else {
            Err(Error::JobsQueueError("jobs queue isn't initialized".into()))
        }
    }
}
