use testcontainers::{
    core::{ContainerPort, Mount, WaitFor},
    Image,
};

pub const GITEA_IMAGE_NAME: &str = "gitea/gitea";
pub const GITEA_IMAGE_TAG: &str = "1.22.2-rootless";
pub const GITEA_SSH_PORT: ContainerPort = ContainerPort::Tcp(2222);
pub const GITEA_HTTPS_PORT: ContainerPort = ContainerPort::Tcp(3000);

#[derive(Debug, Clone)]
pub struct Gitea {
    config_folder: Mount,
    data_folder: Mount,
}

impl Gitea {
    pub fn new(config_folder: impl Into<String>, data_folder: impl Into<String>) -> Self {
        Self {
            config_folder: Mount::bind_mount(config_folder, "/etc/gitea"),
            data_folder: Mount::bind_mount(data_folder, "/var/lib/gitea"),
        }
    }
}

impl Image for Gitea {
    fn name(&self) -> &str {
        GITEA_IMAGE_NAME
    }

    fn tag(&self) -> &str {
        GITEA_IMAGE_TAG
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stdout(format!(
            "Starting new Web server: tcp:0.0.0.0:{}",
            GITEA_HTTPS_PORT.as_u16()
        ))]
    }

    fn mounts(&self) -> impl IntoIterator<Item = &Mount> {
        vec![&self.config_folder, &self.data_folder]
    }

    fn expose_ports(&self) -> &[ContainerPort] {
        &[GITEA_SSH_PORT, GITEA_HTTPS_PORT]
    }
}
