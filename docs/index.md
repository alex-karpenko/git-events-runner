# Introduction

## Briefly

**Git Events Runner** is a Kubernetes operator to trigger `Jobs` with the Git repository content in the job's container.
In other words it provides way to run code inside a Kubernetes cluster based on content of the Git repository commit
in response to changes in the repository or triggered by a webhook.

## What is under the hood

`Git Events Runner` provides several custom resources (CRDs) to define such entities as:

* `Sources` - URI and auth parameters of git repositories which should be watching and using to run `Actions`.
* `Triggers` - conditions and restrictions of changes in `Sources` which triggers `Actions`: which repos, branches or tags to watch and how often.
It includes scheduled and webhook triggers.
* `Actions` - predefined configurations of K8s `Job` to run as reaction to fired  `Trigger`.

Everything is glued by dedicated Kubernetes controller inside a cluster.

## Where it applies

* `Continues Deployment` (CD) process, where Kubernetes resources are involved.
* Running periodic tasks in Kubernetes, based of code in Git repo.
* Triggering tasks in Kubernetes by webhooks, based of code in Git repo.
* Replacement of CI/CD functionality, provided by repository vendor (Github actions for example)
where direct access to Kubernetes or cloud resources is needed but with guarantied permissions restrictions.

> Each item above implies using of approved code from some configured Git repository, branch, tag or even commit.
