use crate::{Error, Result};
use futures::{future::ready, StreamExt};
use k8s_openapi::{api::batch::v1::Job, Metadata};
use kube::api::{ObjectMeta, PostParams};
use kube::ResourceExt;
use kube::{
    runtime::{predicates, reflector, watcher, WatchStreamExt},
    Api, Client,
};
use std::{
    fmt::Debug,
    sync::{Arc, OnceLock},
};
use tracing::{debug, info, warn};

const ACTION_JOB_IDENTITY_LABEL: &str = "git-events-runner.rs/controller-identity";

static JOBS_QUEUE: OnceLock<Arc<JobsQueue>> = OnceLock::new();

pub struct JobsQueue {
    client: Client,
    identity: String,
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
        JOBS_QUEUE
            .set(Arc::new(Self {
                client: client.clone(),
                identity: identity.clone(),
            }))
            .unwrap();

        let jobs_api: Api<Job> = Api::all(client);
        let watcher_config =
            watcher::Config::default().labels(format!("{ACTION_JOB_IDENTITY_LABEL}={}", identity).as_str());
        let (store, writer) = reflector::store();

        let jobs_stream = reflector::reflector(writer, watcher(jobs_api, watcher_config));

        jobs_stream
            .applied_objects()
            .predicate_filter(predicates::generation)
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
                ready(())
            })
            .await
    }

    /// Create or enqueue Jobs.
    /// If jobs queue has enough capacity - starts job immediately,
    /// If there is no capacity right now - put job into the queue
    /// and rely on the watcher to start it.
    pub async fn enqueue(job: Job, ns: &str) -> Result<Job> {
        if let Some(queue) = JOBS_QUEUE.get() {
            debug!(%ns, name = %job.name_any(), "create job");

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

            let jobs_api: Api<Job> = Api::namespaced(queue.client.clone(), ns);
            jobs_api
                .create(&PostParams::default(), &job)
                .await
                .map_err(Error::KubeError)
        } else {
            Err(Error::JobsQueueError("jobs queue isn't initialized".into()))
        }
    }
}
