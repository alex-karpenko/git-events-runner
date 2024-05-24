# Git Events Runner

<p>
<a href="https://github.com/alex-karpenko/git-events-runner/actions/workflows/ci.yaml" rel="nofollow"><img src="https://img.shields.io/github/actions/workflow/status/alex-karpenko/git-events-runner/ci.yaml?label=ci" alt="CI status"></a>
<a href="https://github.com/alex-karpenko/git-events-runner/actions/workflows/audit.yaml" rel="nofollow"><img src="https://img.shields.io/github/actions/workflow/status/alex-karpenko/git-events-runner/audit.yaml?label=audit" alt="Audit status"></a>
<a href="https://github.com/alex-karpenko/git-events-runner/blob/HEAD/LICENSE" rel="nofollow"><img src="https://img.shields.io/crates/l/git-events-runner" alt="License"></a>
</p>

**Git Events Runner** is a Kubernetes operator to trigger `Jobs` with the Git repository content in the job's container.
In other words it provides way to run code inside a Kubernetes cluster based on content of the Git repository commit
in response to changes in the repository or triggered by a webhook.

For details, please refer to the [documentation](https://alex-karpenko.github.io/git-events-runner/).

## What's inside

`Git Events Runner` provides several custom resources (CRDs) to define such entities as:

* `Sources` - URI and auth parameters of git repositories which should be watching and using to run `Actions`.
* `Triggers` - conditions and restrictions of changes in `Sources` which triggers `Actions`: which repos, branches or
  tags to watch and how often.
  It includes scheduled and webhook triggers.
* `Actions` - predefined configurations of K8s `Job` to run as reaction to fired  `Trigger`.

Everything is glued by dedicated Kubernetes controller inside a cluster.

## Where it applies

* Various Continues Deployment (CD) processes, where Kubernetes resources are involved.
* Running periodic tasks in Kubernetes, based on code in Git repo.
* Triggering tasks in Kubernetes by webhooks, based on code in Git repo.
* Replacement of CI/CD functionality, provided by repository vendor (GitHub actions, for example)
  where direct access to Kubernetes or cloud resources is needed but with guarantied permissions restrictions.

## Current state

This is a working beta, and it's under active development now.

## License

This project is licensed under the [MIT license](LICENSE).
