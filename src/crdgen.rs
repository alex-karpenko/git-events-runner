use controllers::Action;
use controllers::GitRepo;
use controllers::ScheduleTrigger;
use controllers::WebhookTrigger;
use kube::CustomResourceExt;

fn main() {
    print!("---\n{}", serde_yaml::to_string(&GitRepo::crd()).unwrap());
    print!(
        "---\n{}",
        serde_yaml::to_string(&ScheduleTrigger::crd()).unwrap()
    );
    print!(
        "---\n{}",
        serde_yaml::to_string(&WebhookTrigger::crd()).unwrap()
    );
    print!("---\n{}", serde_yaml::to_string(&Action::crd()).unwrap());
}
