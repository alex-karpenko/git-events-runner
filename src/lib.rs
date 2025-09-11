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
        serde_yaml_ng::Error,
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
    mod containers;

    use crate::resources::get_all_crds;
    use containers::k3s::{K3S_API_PORT, K3s};
    use ctor::dtor;
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
    use kube::{Api, Client, api::PostParams};
    use rustls::crypto::aws_lc_rs;
    use std::{env, thread};
    use testcontainers::{ContainerAsync, ImageExt as _, runners::AsyncRunner as _};
    use testcontainers_modules::gitea::{GITEA_HTTP_PORT, GITEA_SSH_PORT, Gitea};
    use tokio::{
        runtime::Runtime,
        sync::{Mutex, OnceCell, RwLock},
    };

    const USE_EXISTING_K8S_CONTEXT: &str = "CARGO_TEST_USE_EXISTING_K8S_CONTEXT";
    const GIT_SSH_SERVER_PORT: u16 = 22;
    const GIT_HTTP_SERVER_PORT: u16 = 443;
    const K3S_PORT: u16 = 9443;
    const DOCKER_NETWORK_NAME: &str = "test";
    const DISABLE_CONTAINER_DESTRUCTORS_ENV_NAME: &str = "DISABLE_CONTAINER_DESTRUCTORS";

    static GIT_SERVER_CONTAINER: OnceCell<RwLock<Option<ContainerAsync<Gitea>>>> = OnceCell::const_new();
    static K3S_CLUSTER_CONTAINER: OnceCell<RwLock<Option<ContainerAsync<K3s>>>> = OnceCell::const_new();

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

    pub async fn get_test_git_hostname() -> anyhow::Result<String> {
        let host = get_git_server().await.read().await.as_ref().unwrap().get_host().await?;
        Ok(host.to_string())
    }

    pub async fn get_test_git_ca() -> anyhow::Result<String> {
        let gitea = get_git_server().await.read().await;
        let ca = gitea.as_ref().unwrap().image().tls_ca().unwrap();
        Ok(ca.to_string())
    }

    pub async fn get_test_kube_client() -> anyhow::Result<Client> {
        if std::env::var(USE_EXISTING_K8S_CONTEXT).is_ok() {
            init_crypto_provider().await;
            let client = Client::try_default().await?;
            return Ok(client);
        }

        let guard = get_k3s_cluster().await.read().await;
        let cluster = guard.as_ref().unwrap();
        K3s::get_client(cluster).await
    }

    async fn get_git_server() -> &'static RwLock<Option<ContainerAsync<Gitea>>> {
        GIT_SERVER_CONTAINER
            .get_or_init(|| async {
                let public_key = include_str!("../tests/ssh/test-key-ed25519.pub");
                let container = Gitea::default()
                    .with_tls(true)
                    .with_admin_account("gitea-admin", "gitea-admin", Some(public_key.to_string()))
                    .with_repo(testcontainers_modules::gitea::GiteaRepo::Public("test-1".to_string()))
                    .with_repo(testcontainers_modules::gitea::GiteaRepo::Private("test-2".to_string()))
                    .with_container_name("git-server")
                    .with_mapped_port(GIT_SSH_SERVER_PORT, GITEA_SSH_PORT)
                    .with_mapped_port(GIT_HTTP_SERVER_PORT, GITEA_HTTP_PORT)
                    .with_network(DOCKER_NETWORK_NAME)
                    .start()
                    .await
                    .unwrap();
                RwLock::new(Some(container))
            })
            .await
    }

    async fn get_k3s_cluster() -> &'static RwLock<Option<ContainerAsync<K3s>>> {
        K3S_CLUSTER_CONTAINER
            .get_or_init(|| async {
                init_crypto_provider().await;

                // Put kubeconfig into target out directory
                let out_dir =
                    env::var("OUT_DIR").expect("`OUT_DIR` environment variable isn`t set, use Cargo to run build");

                // Create k3s container
                let runtime_folder = format!("{out_dir}/k3s-runtime");
                tokio::fs::create_dir_all(&runtime_folder).await.unwrap();

                let container = K3s::new(runtime_folder)
                    .with_container_name("k3s")
                    .with_userns_mode("host")
                    .with_privileged(true)
                    .with_mapped_port(K3S_PORT, K3S_API_PORT)
                    .with_network(DOCKER_NETWORK_NAME)
                    .start()
                    .await
                    .unwrap();
                // and apply all CRDs into the cluster
                let client = K3s::get_client(&container).await.unwrap();
                create_all_crds(client).await.unwrap();

                RwLock::new(Some(container))
            })
            .await
    }

    async fn create_all_crds(client: Client) -> anyhow::Result<()> {
        let pp = PostParams::default();
        let crd_api = Api::<CustomResourceDefinition>::all(client);
        for crd in get_all_crds() {
            crd_api.create(&pp, &crd).await?;
        }

        Ok(())
    }

    #[dtor]
    fn shutdown_test_containers() {
        static LOCK: Mutex<()> = Mutex::const_new(());

        if env::var(DISABLE_CONTAINER_DESTRUCTORS_ENV_NAME).is_err() {
            let _ = thread::spawn(|| {
                Runtime::new().unwrap().block_on(async {
                    let _guard = LOCK.lock().await;

                    if let Some(k3s) = K3S_CLUSTER_CONTAINER.get() {
                        let mut k3s = k3s.write().await;
                        if k3s.is_some() {
                            let old = (*k3s).take().unwrap();
                            old.stop().await.unwrap();
                            old.rm().await.unwrap();
                            *k3s = None;
                        }
                    }

                    if let Some(git) = GIT_SERVER_CONTAINER.get() {
                        let mut git = git.write().await;
                        if git.is_some() {
                            let old = (*git).take().unwrap();
                            old.stop().await.unwrap();
                            old.rm().await.unwrap();
                            *git = None;
                        }
                    }
                });
            })
            .join();
        }
    }
}
