use kubert::{lease::ClaimParams, LeaseManager};
use kubert_k8s_openapi::{
    api::coordination::v1 as coordv1, apimachinery::pkg::apis::meta::v1 as metav1,
};
use kubert_kube::Client;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{debug, error, info};
use uuid::Uuid;

const DEFAULT_LEADER_LOCK_LEASE_NAME: &str = "git-events-runner-leader-lock";
const DEFAULT_LEADER_LOCK_LEASE_DERATION_SEC: u64 = 10;
const DEFAULT_LEADER_LOCK_LEASE_GRACE_SEC: u64 = 5;

pub struct LeaderLock {
    client: Client,
    identity: String,
    namespace: String,
    name: String,
    claimed: bool,
}

impl LeaderLock {
    pub async fn new(namespace: Option<String>) -> Self {
        let client = Client::try_default()
            .await
            .expect("failed to create kube Client");
        let namespace = namespace.unwrap_or("default".into());
        let name: String = DEFAULT_LEADER_LOCK_LEASE_NAME.into();

        // Create Lease
        let api = kubert_kube::Api::<coordv1::Lease>::namespaced(client.clone(), &namespace);
        let lease = api
            .create(
                &Default::default(),
                &coordv1::Lease {
                    metadata: metav1::ObjectMeta {
                        name: Some(name.clone()),
                        namespace: Some(namespace.clone()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await;

        if let Err(err) = lease {
            if let kubert_kube::Error::Api(err) = err {
                if err.code != 409 {
                    error!("unable to create leader lock {namespace}/{name}: {err:?}");
                }
            } else {
                debug!("Leader lock Lease {namespace}/{name} already exists");
            }
        } else {
            info!("Leader lock Lease {namespace}/{name} has been created");
        }

        Self {
            client,
            namespace,
            identity: Uuid::new_v4().to_string(),
            name,
            claimed: false,
        }
    }

    pub async fn wait_for_change(
        &mut self,
        mut shutdown_channel: watch::Receiver<bool>,
    ) -> Result<bool, ()> {
        let api =
            kubert_kube::Api::<coordv1::Lease>::namespaced(self.client.clone(), &self.namespace);
        let manager = LeaseManager::init(api, self.name.clone())
            .await
            .expect("unable to create LeaseManager instance");
        let params = ClaimParams {
            lease_duration: Duration::from_secs(DEFAULT_LEADER_LOCK_LEASE_DERATION_SEC),
            renew_grace_period: Duration::from_secs(DEFAULT_LEADER_LOCK_LEASE_GRACE_SEC),
        };

        let (mut claims, task) = manager
            .spawn(&self.identity, params)
            .await
            .expect("unable to create LeaseManager");

        loop {
            {
                let claim = claims.borrow_and_update();
                let claimed = claim.is_current_for(&self.identity);
                if claimed != self.claimed {
                    self.claimed = claimed;
                    return Ok(claimed);
                }
            }

            tokio::select! {
                biased;
                _ = shutdown_channel.changed() => {
                    let _ = task.await.expect("failed LeaseManager task");
                    return Err(())
                }
                res = claims.changed() => {
                    if res.is_err() {
                        let _ = task.await.expect("failed LeaseManager task");
                        return Err(())
                    } else {
                        continue
                    }
                }
            }
        }
    }

    pub fn is_locked(&self) -> bool {
        self.claimed
    }
}
