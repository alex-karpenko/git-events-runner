#![deny(unsafe_code, warnings, missing_docs)]
//! All `git-events-runner` routines

pub mod cache;
pub mod cli;
pub mod config;
pub mod controller;
pub mod jobs;
pub mod resources;
pub mod signals;
pub mod web;

use thiserror::Error;

/// Convenient alias for Result with our Error type
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Error, Debug)]
/// git-events-runner specific errors
pub enum Error {
    /// All errors from the Kubernetes API of `kube` crate
    #[error("Kube Error: {0}")]
    KubeError(
        #[source]
        #[from]
        kube::Error,
    ),

    /// Errors related to leader election management
    #[error("LeaseManager Error: {0}")]
    LeaseManagerError(
        #[source]
        #[from]
        kube_lease_manager::LeaseManagerError,
    ),

    /// Controller runtime specific error related to reconciling
    #[error("Finalizer Error: {0}")]
    FinalizerError(
        #[source]
        #[from]
        Box<kube::runtime::finalizer::Error<Error>>,
    ),

    /// Errors related to retrieving git repo content,
    /// or auth-related access issues
    #[error("GitRepo Access Error: {0}")]
    GitrepoAccessError(
        #[source]
        #[from]
        git2::Error,
    ),

    /// Error during decoding of the secrets' base64 content
    #[error("GitRepo Secret Decoding Error: {0}")]
    SecretDecodingError(String),

    /// Unable to deserialize runtime config due to wrong schema
    #[error("RuntimeConfig ConfigMap format Error")]
    RuntimeConfigFormatError,

    /// Unable to access a file during hash calculation
    #[error("Trigger file access IO error: {0}")]
    TriggerFileAccessError(
        #[source]
        #[from]
        std::io::Error,
    ),

    /// Unable to walk thought directories during hash calculation
    #[error("Trigger dir walk error: {0}")]
    TriggerDirWalkError(String),

    /// Incorrect file pattern for hash calculation
    #[error("Trigger file pattern error: {0}")]
    TriggerFilePatternError(
        #[source]
        #[from]
        globwalk::GlobError,
    ),

    /// Error during SACS task scheduling
    #[error("Task scheduler error: {0}")]
    SchedulerError(
        #[source]
        #[from]
        sacs::Error,
    ),

    /// Unable to deserialize runtime config due to incorrect yaml format
    #[error("Config deserialization error: {0}")]
    ConfigDeserializationError(
        #[source]
        #[from]
        serde_yaml::Error,
    ),

    /// Unable to retrieve a resource from cache when it should be present in cache
    #[error("Resource not found in cache: {0}")]
    ResourceNotFoundError(String),

    /// Error related to processing of the Jobs queue
    #[error("JobsQueue error: {0}")]
    JobsQueueError(String),

    /// Unable to parse request rate limit string
    #[error("Incorrect request rate limit parameter: {0}")]
    InvalidRequestRateLimit(String),
}

#[cfg(test)]
mod tests {
    pub mod containers;

    use containers::{gitea::Gitea, k3s::K3s};
    use ctor::dtor;
    use kube::Client;
    use rustls::crypto::aws_lc_rs;
    use std::{env, thread};
    use testcontainers::ContainerAsync;
    use tokio::{runtime::Runtime, sync::OnceCell};

    static mut GIT_SERVER_CONTAINER: OnceCell<ContainerAsync<Gitea>> = OnceCell::const_new();
    static mut K3S_CLUSTER_CONTAINER: OnceCell<ContainerAsync<K3s>> = OnceCell::const_new();

    pub async fn init_crypto_provider() {
        static CRYPTO_PROVIDER_INITIALIZED: OnceCell<()> = OnceCell::const_new();

        CRYPTO_PROVIDER_INITIALIZED
            .get_or_init(|| async {
                aws_lc_rs::default_provider()
                    .install_default()
                    .expect("Failed to install rustls crypto provider");
            })
            .await;
    }

    #[allow(dead_code)]
    pub async fn get_test_git_hostname() -> String {
        get_git_server().await.get_host().await.unwrap().to_string()
    }

    pub async fn get_test_kube_client() -> Client {
        K3s::get_client(get_k3s_cluster().await).await.unwrap()
    }

    #[allow(unsafe_code)]
    async fn get_git_server() -> &'static ContainerAsync<Gitea> {
        unsafe {
            GIT_SERVER_CONTAINER
                .get_or_init(|| async {
                    let out_dir =
                        env::var("OUT_DIR").expect("`OUT_DIR` environment variable isn`t set, use Cargo to run build");
                    containers::run_git_server(&format!("{out_dir}/gitea-runtime"))
                        .await
                        .unwrap()
                })
                .await
        }
    }

    #[allow(unsafe_code)]
    async fn get_k3s_cluster() -> &'static ContainerAsync<K3s> {
        unsafe {
            K3S_CLUSTER_CONTAINER
                .get_or_init(|| async {
                    let out_dir =
                        env::var("OUT_DIR").expect("`OUT_DIR` environment variable isn`t set, use Cargo to run build");
                    containers::run_k3s_cluster(&format!("{out_dir}/k3s-runtime"))
                        .await
                        .unwrap()
                })
                .await
        }
    }

    #[dtor]
    #[allow(unsafe_code)]
    fn shutdown_test_containers() {
        let _ = thread::spawn(move || {
            Runtime::new().unwrap().block_on(async {
                unsafe {
                    if let Some(k3s) = K3S_CLUSTER_CONTAINER.take() {
                        k3s.stop().await.unwrap();
                        k3s.rm().await.unwrap()
                    }

                    if let Some(git) = GIT_SERVER_CONTAINER.take() {
                        git.stop().await.unwrap();
                        git.rm().await.unwrap()
                    }
                }
            });
        })
        .join();
    }
}
