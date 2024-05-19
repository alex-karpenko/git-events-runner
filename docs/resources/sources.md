# Sources

Describes a source to watch on changes in it. [GitRepo](#gitrepo) and [ClusterGitRepo](#clustergitrepo) are implemented
so far.

Below are examples of each resource kind with detailed comments.

There are only two differences between these resources:

1. ClusterGitRepo is a cluster wide resource, but GitRepo is namespaced one, sounds pretty obvious.
2. GitRepo can refer Secrets in its own namespace only, whereas ClusterGitRepo can use Secrets in any namespace (if this
   is permitted by one of its roles).

## GitRepo

```yaml title="GitRepo"
--8<-- "docs/examples/gitrepo.yaml"
```

## ClusterGitRepo

```yaml title="ClusterGitRepo"
--8<-- "docs/examples/clustergitrepo.yaml"
```
