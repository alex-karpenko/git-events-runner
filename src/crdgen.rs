use controllers::Action;
use controllers::GitRepo;
use controllers::Trigger;
use kube::CustomResourceExt;

fn main() {
    print!("---\n{}", serde_yaml::to_string(&GitRepo::crd()).unwrap());
    print!("---\n{}", serde_yaml::to_string(&Trigger::crd()).unwrap());
    print!("---\n{}", serde_yaml::to_string(&Action::crd()).unwrap());
}
