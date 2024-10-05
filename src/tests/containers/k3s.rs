use kube::{
    config::{KubeConfigOptions, Kubeconfig},
    Config,
};
use rustls::crypto::CryptoProvider;
use std::{borrow::Cow, io, path::Path};
use testcontainers::{
    core::{ContainerPort, Mount, WaitFor},
    ContainerAsync, Image,
};

pub const K3S_IMAGE_NAME: &str = "rancher/k3s";
pub const K3S_IMAGE_TAG: &str = "v1.31.1-k3s1";
pub const K3S_API_PORT: ContainerPort = ContainerPort::Tcp(6443);

#[derive(Debug, Clone)]
pub struct K3s {
    kubeconfig_folder: Mount,
}

impl Image for K3s {
    fn name(&self) -> &str {
        K3S_IMAGE_NAME
    }

    fn tag(&self) -> &str {
        K3S_IMAGE_TAG
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stderr("Node controller sync successful")]
    }

    fn env_vars(&self) -> impl IntoIterator<Item = (impl Into<Cow<'_, str>>, impl Into<Cow<'_, str>>)> {
        vec![
            // (
            //     String::from("K3S_KUBECONFIG_OUTPUT"),
            //     String::from(self.kubeconfig_folder.target().unwrap()),
            // ),
            (String::from("K3S_KUBECONFIG_MODE"), String::from("644")),
        ]
    }

    fn mounts(&self) -> impl IntoIterator<Item = &Mount> {
        vec![&self.kubeconfig_folder]
    }

    fn cmd(&self) -> impl IntoIterator<Item = impl Into<Cow<'_, str>>> {
        vec![
            "server",
            "--snapshotter=native",
            "--disable-network-policy",
            "--disable=traefik",
            "--disable=coredns",
            "--disable=servicelb",
            "--disable=local-storage",
            "--disable=metrics-server",
            "--disable-helm-controller",
            // "--disable-agent",
        ]
    }

    fn expose_ports(&self) -> &[ContainerPort] {
        &[K3S_API_PORT]
    }
}

impl K3s {
    pub fn new(kubeconfig_folder: impl Into<String>) -> Self {
        Self {
            kubeconfig_folder: Mount::bind_mount(kubeconfig_folder, "/etc/rancher/k3s/"),
        }
    }

    async fn get_kubeconfig(&self) -> io::Result<String> {
        let k3s_conf_file_path = self.kubeconfig_folder.source().unwrap();
        let k3s_conf_file_path = Path::new(k3s_conf_file_path).join("k3s.yaml");
        tokio::fs::read_to_string(k3s_conf_file_path).await
    }

    pub async fn get_client(container: &ContainerAsync<K3s>) -> anyhow::Result<kube::Client> {
        if CryptoProvider::get_default().is_none() {
            rustls::crypto::ring::default_provider()
                .install_default()
                .expect("Error initializing rustls provider");
        }

        let conf_yaml = container.image().get_kubeconfig().await?;
        let mut config = Kubeconfig::from_yaml(&conf_yaml).expect("Error loading kube config");

        let port = container.get_host_port_ipv4(K3S_API_PORT).await?;
        config.clusters.iter_mut().for_each(|cluster| {
            if let Some(server) = cluster.cluster.as_mut().and_then(|c| c.server.as_mut()) {
                *server = format!("https://127.0.0.1:{}", port)
            }
        });

        let client_config = Config::from_custom_kubeconfig(config, &KubeConfigOptions::default()).await?;

        Ok(kube::Client::try_from(client_config)?)
    }
}
