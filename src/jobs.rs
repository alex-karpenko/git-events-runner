use crate::resources::action::ACTION_JOB_IDENTITY_LABEL;
use futures::{future::ready, StreamExt};
use k8s_openapi::{api::batch::v1::Job, Metadata};
use kube::{
    runtime::{predicates, watcher, WatchStreamExt},
    Api, Client,
};
use tracing::{debug, info, warn};

/// Watch and log changes of OURS K8s jobs.
/// We identify jobs as ours by label (ACTION_JOB_IDENTITY_LABEL) with value of our own identity.
/// So each controller instance watches it's own jobs oly.
pub async fn watch(client: Client, identity: String) {
    let jobs_api: Api<Job> = Api::all(client.clone());
    let watcher_config = watcher::Config::default().labels(format!("{ACTION_JOB_IDENTITY_LABEL}={identity}").as_str());
    let jobs_stream = watcher(jobs_api, watcher_config);

    jobs_stream
        .applied_objects()
        .predicate_filter(predicates::generation)
        .for_each(|job| {
            if let Ok(job) = job {
                let name = job.metadata().name.clone().unwrap();
                let ns = job.metadata().namespace.clone().unwrap();
                let status = job.status.unwrap();

                debug!("{status:?}");
                // Since we run jobs with parallelism=1 and w/o restarts,
                // we look for just `succeed` or `failed` in the status,
                // but not for counters under them
                if status.succeeded.is_some() {
                    info!("Job {ns}/{name} completed successfully.");
                } else if status.failed.is_some() {
                    warn!("Job {ns}/{name} failed.");
                }
            }
            ready(())
        })
        .await
}
