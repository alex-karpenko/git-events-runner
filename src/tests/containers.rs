pub mod gitea;
pub mod k3s;

use gitea::{Gitea, GITEA_HTTPS_PORT, GITEA_SSH_PORT};
use k3s::{K3s, K3S_API_PORT};
use testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt as _};

pub const GIT_SSH_SERVER_PORT: u16 = 22;
pub const GIT_HTTP_SERVER_PORT: u16 = 443;
pub const K3S_PORT: u16 = 9443;
pub const DOCKER_NETWORK_NAME: &str = "test";

pub(crate) async fn run_git_server(gitea_base_dir: &String) -> anyhow::Result<ContainerAsync<Gitea>> {
    let config_dir = format!("{gitea_base_dir}/config");
    let data_dir = format!("{gitea_base_dir}/data");

    let container = Gitea::new(config_dir, data_dir)
        .with_container_name("git-server")
        .with_mapped_port(GIT_SSH_SERVER_PORT, GITEA_SSH_PORT)
        .with_mapped_port(GIT_HTTP_SERVER_PORT, GITEA_HTTPS_PORT)
        .with_network(DOCKER_NETWORK_NAME)
        .start()
        .await?;

    Ok(container)
}

pub(crate) async fn run_k3s_cluster(k3s_base_dir: &String) -> anyhow::Result<ContainerAsync<K3s>> {
    tokio::fs::create_dir_all(k3s_base_dir).await?;

    let container = K3s::new(k3s_base_dir)
        .with_container_name("k3s")
        .with_userns_mode("host")
        .with_privileged(true)
        .with_mapped_port(K3S_PORT, K3S_API_PORT)
        .with_network(DOCKER_NETWORK_NAME)
        .start()
        .await?;

    Ok(container)
}
