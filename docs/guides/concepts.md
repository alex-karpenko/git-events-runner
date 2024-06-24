# Core concepts and components

## Controller

Typically, it's deployed to Kubernetes cluster as a Deployment resource using pre-built Docker image.
Its responsibility is to:

* **Watch on [custom resources](../resources/sources.md)** (triggers, actions, sources) and hold a local resources'
  state (for performance purpose).
* Watch on changes of [ScheduleTrigger](../resources/triggers.md#scheduletrigger) and **update schedules** if it's
  changed.
* **Periodically run tasks** (according to triggers' schedules) to check for changes of ScheduleTriggers' sources.
* Run web server to **handle webhook requests** to check for changes of WebhookTriggers' sources.
* Create Kubernetes Job to **apply configured action** if a triggers' source is changed (or in some other circumstances
  depending on triggers' config).
* Watch on Jobs which controller has run to provide **Jobs' final state** into the logs.
* Run **utility web server** to handle requests for liveness and readiness probes, and to provide metrics.
* Watch on ConfigMap with dynamic controllers' config and **reload dynamic configuration** in case of changes.
* Hold leader lease and do **leader election** with respect to all pods of the controller.
* Watch on signals to **shut down gracefully** if termination was requested.

### High availability

It's possible to run multiple instances of the controller.
Moreover, such a config is preferred for production environments.
Controller manages single instance where ScheduleTriggers should be executed, it uses Kubernetes Lease resource to
ensure fast and reliable leaders' election.

If at least two controllers' instances are running, in case of failures (pod evicted, node crashed, etc.) there is
a builtin mechanism to smoothly move triggers' scheduling to another available instance.

One more point to pay attention to is WebhookTriggers' availability.
Since this kind of trigger is actually a web server running on all controllers' instances.
Having at least two controllers' instances helps do workload balancing and provides a classic mechanism of failure
protection because Kubernetes Service always forwards traffic to available pods only.

## Custom resources

Detailed references to all resources is in the separate section [Resources](../resources/sources.md).
But few aspects should be mentioned:

### Active vs passive resources

ScheduleTrigger is the only active resource that has its own state outside the cluster and initiates periodic tasks
for checking sources.

All other resources are either a passive WebhookTrigger (it waits for external http requests to do any activity) or
supplementary for triggers (sources and actions - triggers use them to interact with repos or run jobs).

### Namespaced vs. cluster-wide resources

Besides [introduction](../intro/overview.md#namespaced-vs-cluster-resources), we have to know the following:

* ClusterGitRepo can refer secrets in any namespace, but allowing access to secrets outside of controller's namespace is
  the responsibility of the cluster administrator.
  Provided Helm chart may facilitate this.
* ClusterAction can override the source used in its Job (this is a feature of both actions resources), but it can use
  ClusterGitRepo only to avoid cross-namespace access to GitRepo in multi-tenant environments.

### CRDs manifests

[`git-events-runner` Helm chart](https://github.com/alex-karpenko/helm-charts/tree/main/charts/git-events-runner)
contains the latest actual bundle of CRD manifests.

Besides that, it's possible to get CRDs right from the controllers binary. The easiest way is to run controller's Docker
image with `crds` parameter:

```shell
docker run --rm ghcr.io/alex-karpenko/git-events-runner/git-events-runner:latest crds
```

Use any valid version/tag instead of `latest` to get the specific version of manifests, if necessary.

## Images and binaries

Git Events Runner uses three types of images to run:

* **git-events-runner**: pre-built image with single controller's binary, without shell, package managers, etc.;
* **gitrepo-cloner**: pre-built image with single binary of utility to safely clone git repositories, it's used in the
  init container of action Jobs, without shell, package managers, etc.;
* **action-worker**: Job's runtime image, which contains everything you need to do actual work with cloned code.

As for action-worker, you can use any image you need: existing one from any repository (`bash` or `ubuntu` for example),
anything built by yourself or default one provided in this project.
Anyway, what command to run on start you provide in actions' config.

There is default **action-worker** image built from
this [Dockerfile](https://github.com/alex-karpenko/git-events-runner/blob/main/docker-build/action-worker.dockerfile),
it's based on Ubuntu 22.04 and includes the set of typically used CD tools: Python, AWS CLI, Helm, kubectl, curl.

All Dockerfiles of this project are
there: [https://github.com/alex-karpenko/git-events-runner/tree/main/docker-build](https://github.com/alex-karpenko/git-events-runner/tree/main/docker-build)

## Action Job internals

When a trigger discovers that repo was changed, it creates a
classic [Kubernetes Job](https://kubernetes.io/docs/concepts/workloads/controllers/job/) to run Action because action is
a single-shot piece of work.

[Action resources](../resources/actions.md) provide a way to slightly customize jobs, but in general configuration is:

* Single `initContainer` runs `gitrepo-cloner` image to clone needed repo/commit to predefined volume.
* Single `container` runs `action-worker` image to do actual work.
* Both containers mount shared `emptyDir` volume with content of the git repo/commit, worker container uses it as a
  Pods' workdir.
* Worker container has set of predefined environment variables with prefix in names (default is `ACTION_JOB_`) with
  some actual runtime parameters that can be useful in workers.
  Look into the detailed [actions reference](../resources/actions.md) for the full list of variables with descriptions.
* Job has a zero backoff limit and single parallelism to ensure running only once.

### Jobs' environment variables

As mentioned above, `ACTION_JOB_` is just default prefix and may be configured to anything else.

| Variable name                       | Mandatory | Description                                                                                |
|-------------------------------------|-----------|--------------------------------------------------------------------------------------------|
| ACTION_JOB_WORKDIR                  | Yes       | Worker container workdir (folder where source repo is cloned)                              |
| ACTION_JOB_TRIGGER_KIND             | Yes       | Kind of the trigger (ScheduleTrigger, WebhookTrigger)                                      |     
| ACTION_JOB_TRIGGER_NAME             | Yes       | Name of the trigger.                                                                       |
| ACTION_JOB_TRIGGER_SOURCE_KIND      | Yes       | Kind of the triggers' source (Gitrepo, ClusterGitRepo)                                     |     
| ACTION_JOB_TRIGGER_SOURCE_NAME      | Yes       | Name of the triggers' source.                                                              |
| ACTION_JOB_TRIGGER_SOURCE_NAMESPACE | No        | Namespace of the triggers' source (for GitRepo only).                                      |
| ACTION_JOB_TRIGGER_SOURCE_COMMIT    | Yes       | Commit hash which is cloned into workdir.                                                  |
| ACTION_JOB_TRIGGER_SOURCE_REF_TYPE  | Yes       | Type of the triggers' repo reference which is configured to watch on (branch, tag, commit) |
| ACTION_JOB_TRIGGER_SOURCE_REF_NAME  | Yes       | Name of the triggers' repo reference.                                                      |
| ACTION_JOB_ACTION_SOURCE_KIND       | No        | Kind of the actions' overridden source.                                                    |
| ACTION_JOB_ACTION_SOURCE_NAME       | No        | Name of the actions' overridden source.                                                    |
| ACTION_JOB_ACTION_SOURCE_NAMESPACE  | No        | Namespace on the actions' overridden source.                                               |
| ACTION_JOB_ACTION_SOURCE_REF_TYPE   | No        | Reference type of the actions' overridden source.                                          |
| ACTION_JOB_ACTION_SOURCE_REF_NAME   | No        | Name of the actions' overridden source.                                                    |

Details about `source override` feature can be found in the [detailed documentation](../resources/actions.md). 