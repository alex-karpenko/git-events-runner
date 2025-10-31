# Changelog

All notable changes to this project will be documented in this file.

## [0.4.8] - 2025-10-31
### Details
#### Fixed
- Update dependencies to fix known vulnerabilities

## [0.4.7] - 2025-09-12
### Details
#### Changed
- Maintenance release to update dependencies by @alex-karpenko in [#62](https://github.com/alex-karpenko/git-events-runner/pull/62)

## [0.4.6] - 2025-08-23
### Details
#### Changed
- Maintenance release to update dependencies by @alex-karpenko in [#60](https://github.com/alex-karpenko/git-events-runner/pull/60)

## [0.4.5] - 2025-06-21
### Details
#### Changed
- Update dependencies by @alex-karpenko in [#58](https://github.com/alex-karpenko/git-events-runner/pull/58)

#### Fixed
- Fix lint warnings - use string interpolation in format strings by @alex-karpenko in [#56](https://github.com/alex-karpenko/git-events-runner/pull/56)

## [0.4.4] - 2025-04-12
### Details
#### Fixed
- Update mkdocs dependencies by @alex-karpenko in [#54](https://github.com/alex-karpenko/git-events-runner/pull/54)
- Update dependencies to fix known vulnerabiliries by @alex-karpenko in [#55](https://github.com/alex-karpenko/git-events-runner/pull/55)

## [0.4.3] - 2025-03-29
### Details
#### Fixed
- Bump dependencies to fix vulnerablities by @alex-karpenko in [#53](https://github.com/alex-karpenko/git-events-runner/pull/53)

## [0.4.2] - 2025-03-14
### Details
#### Changed
- Update dependencies by @alex-karpenko in [#52](https://github.com/alex-karpenko/git-events-runner/pull/52)

## [0.4.1] - 2025-02-15
### Details
#### Changed
- Update some dependencies to the latest versions by @alex-karpenko in [#51](https://github.com/alex-karpenko/git-events-runner/pull/51)

#### Fixed
- Fix failing CI integration tests by @alex-karpenko in [#48](https://github.com/alex-karpenko/git-events-runner/pull/48)
- Update dependencies to fix security warnings by @alex-karpenko in [#50](https://github.com/alex-karpenko/git-events-runner/pull/50)

## [0.4.0] - 2025-01-04
### Details
#### Breaking changes
- Increase MK8SV to 1.28 (update dependencies) by @alex-karpenko in [#46](https://github.com/alex-karpenko/git-events-runner/pull/46)

#### Changed
- More complex cron expressions (including TZ) by @alex-karpenko in [#45](https://github.com/alex-karpenko/git-events-runner/pull/45)

#### Fixed
- Update mkdocs dependencies to fix security findings by @alex-karpenko in [#47](https://github.com/alex-karpenko/git-events-runner/pull/47)

## [0.3.3] - 2024-12-06
### Details
#### Changed
- Move git unit tests to testcontainers-modules crate by @alex-karpenko in [#43](https://github.com/alex-karpenko/git-events-runner/pull/43)

#### Fixed
- Fix lint warnings by @alex-karpenko in [#42](https://github.com/alex-karpenko/git-events-runner/pull/42)
- Update vulnerable dependencies by @alex-karpenko in [#44](https://github.com/alex-karpenko/git-events-runner/pull/44)

## [0.3.2] - 2024-10-12
### Details
#### Changed
- Update dependencies - kube, kube-lease-manager by @alex-karpenko in [#41](https://github.com/alex-karpenko/git-events-runner/pull/41)
- Improve unit tests with built-in containers support by @alex-karpenko in [#39](https://github.com/alex-karpenko/git-events-runner/pull/39)

#### Fixed
- Fix fetching of repos with SSH or Git URI schemas by @alex-karpenko in [#40](https://github.com/alex-karpenko/git-events-runner/pull/40)

## [0.3.1] - 2024-10-01
### Details
#### Changed
- Logs in JSON format by @alex-karpenko in [#38](https://github.com/alex-karpenko/git-events-runner/pull/38)

## [0.3.0] - 2024-09-28
### Details
#### Added
- Implement web-hooks requests rate limiting by @alex-karpenko in [#35](https://github.com/alex-karpenko/git-events-runner/pull/35)
- Implement TLS listener for webhooks by @alex-karpenko in [#36](https://github.com/alex-karpenko/git-events-runner/pull/36)

## [0.2.4] - 2024-09-21
### Details
#### Changed
- Update dependencies by @alex-karpenko in [#30](https://github.com/alex-karpenko/git-events-runner/pull/30)
- Improve documentation of the code by @alex-karpenko in [#32](https://github.com/alex-karpenko/git-events-runner/pull/32)
- Update dependencies by @alex-karpenko in [#33](https://github.com/alex-karpenko/git-events-runner/pull/33)
- Update kube to v0.95, k8s-openapi to v0.23, kube-lease-manager to v0.4 by @alex-karpenko in [#34](https://github.com/alex-karpenko/git-events-runner/pull/34)

#### Fixed
- Fix flapping test reconcile_schedule_trigger_should_set_idle_status by @alex-karpenko in [#31](https://github.com/alex-karpenko/git-events-runner/pull/31)

## [0.2.3] - 2024-07-19
### Details
#### Fixed
- Update dependencies to fix security findings by @alex-karpenko in [#29](https://github.com/alex-karpenko/git-events-runner/pull/29)

## [0.2.2] - 2024-07-08
### Details
#### Fixed
- Fix large default leader lock grace interval by @alex-karpenko in [#25](https://github.com/alex-karpenko/git-events-runner/pull/25)
- Update to use kube-lease-manager instead of kubert crate by @alex-karpenko in [#28](https://github.com/alex-karpenko/git-events-runner/pull/28)

## [0.2.1] - 2024-06-24
### Details
#### Added
- Implement runtime metrics and imorove traces by @alex-karpenko in [#23](https://github.com/alex-karpenko/git-events-runner/pull/23)

#### Changed
- Improve unit and integration testing by @alex-karpenko in [#21](https://github.com/alex-karpenko/git-events-runner/pull/21)

#### Fixed
- Fix security findings by @alex-karpenko in [#22](https://github.com/alex-karpenko/git-events-runner/pull/22)
- Update dependencies by @alex-karpenko in [#24](https://github.com/alex-karpenko/git-events-runner/pull/24)

## [0.2.0] - 2024-06-15
### Details
#### Breaking changes
- Implement glob file pattern matching instead of the single file name specification in the triggers by @alex-karpenko in [#14](https://github.com/alex-karpenko/git-events-runner/pull/14)

#### Added
- Implement actionJob config with additional labels and annotations by @alex-karpenko in [#10](https://github.com/alex-karpenko/git-events-runner/pull/10)
- Implement actinJob config with affinity, nodeSelector and tolerations rules by @alex-karpenko in [#11](https://github.com/alex-karpenko/git-events-runner/pull/11)
- Implement job execution time limit (active_deadline_seconds) by @alex-karpenko in [#12](https://github.com/alex-karpenko/git-events-runner/pull/12)
- Implement limitation of the running jobs number by @alex-karpenko in [#13](https://github.com/alex-karpenko/git-events-runner/pull/13)
- Add jobs requeueing when config changed by @alex-karpenko in [#17](https://github.com/alex-karpenko/git-events-runner/pull/17)

#### Changed
- Make documentation versioned by @alex-karpenko in [#15](https://github.com/alex-karpenko/git-events-runner/pull/15)
- Improve logging, make it more formal and short, with relevant info only by @alex-karpenko in [#16](https://github.com/alex-karpenko/git-events-runner/pull/16)

#### Fixed
- Update documentation to reflect changes in Helm chart by @alex-karpenko in [#9](https://github.com/alex-karpenko/git-events-runner/pull/9)
- Documentation deployment workflow by @alex-karpenko
- Fix grammar, syntax and lint warnings by @alex-karpenko in [#19](https://github.com/alex-karpenko/git-events-runner/pull/19)

## [0.1.1] - 2024-06-01
### Details
#### Added
- Add subcommand to dump default dynamic config by @alex-karpenko in [#8](https://github.com/alex-karpenko/git-events-runner/pull/8)

#### Changed
- Bump requests from 2.31.0 to 2.32.2 by @dependabot[bot] in [#4](https://github.com/alex-karpenko/git-events-runner/pull/4)
- Push images to docker.io registry by @alex-karpenko in [#6](https://github.com/alex-karpenko/git-events-runner/pull/6)

#### Fixed
- Fix links in the README by @alex-karpenko
- Change action-worker base image to Ubuntu 24.04 by @alex-karpenko in [#7](https://github.com/alex-karpenko/git-events-runner/pull/7)

## New Contributors
* @dependabot[bot] made their first contribution in [#4](https://github.com/alex-karpenko/git-events-runner/pull/4)

## [0.1.0] - 2024-05-24
### Details
#### Changed
- First beta implementation by @alex-karpenko in [#2](https://github.com/alex-karpenko/git-events-runner/pull/2)

#### Fixed
- Fix error in release workflow by @alex-karpenko in [#5](https://github.com/alex-karpenko/git-events-runner/pull/5)
- Fix release workflow permissions by @alex-karpenko

#### Removed
- Remove changelog updating from the release workflow by @alex-karpenko

## New Contributors
* @alex-karpenko made their first contribution

[0.4.8]: https://github.com/alex-karpenko/git-events-runner/compare/v0.4.7..v0.4.8
[0.4.7]: https://github.com/alex-karpenko/git-events-runner/compare/v0.4.6..v0.4.7
[0.4.6]: https://github.com/alex-karpenko/git-events-runner/compare/v0.4.5..v0.4.6
[0.4.5]: https://github.com/alex-karpenko/git-events-runner/compare/v0.4.4..v0.4.5
[0.4.4]: https://github.com/alex-karpenko/git-events-runner/compare/v0.4.3..v0.4.4
[0.4.3]: https://github.com/alex-karpenko/git-events-runner/compare/v0.4.2..v0.4.3
[0.4.2]: https://github.com/alex-karpenko/git-events-runner/compare/v0.4.1..v0.4.2
[0.4.1]: https://github.com/alex-karpenko/git-events-runner/compare/v0.4.0..v0.4.1
[0.4.0]: https://github.com/alex-karpenko/git-events-runner/compare/v0.3.3..v0.4.0
[0.3.3]: https://github.com/alex-karpenko/git-events-runner/compare/v0.3.2..v0.3.3
[0.3.2]: https://github.com/alex-karpenko/git-events-runner/compare/v0.3.1..v0.3.2
[0.3.1]: https://github.com/alex-karpenko/git-events-runner/compare/v0.3.0..v0.3.1
[0.3.0]: https://github.com/alex-karpenko/git-events-runner/compare/v0.2.4..v0.3.0
[0.2.4]: https://github.com/alex-karpenko/git-events-runner/compare/v0.2.3..v0.2.4
[0.2.3]: https://github.com/alex-karpenko/git-events-runner/compare/v0.2.2..v0.2.3
[0.2.2]: https://github.com/alex-karpenko/git-events-runner/compare/v0.2.1..v0.2.2
[0.2.1]: https://github.com/alex-karpenko/git-events-runner/compare/v0.2.0..v0.2.1
[0.2.0]: https://github.com/alex-karpenko/git-events-runner/compare/v0.1.1..v0.2.0
[0.1.1]: https://github.com/alex-karpenko/git-events-runner/compare/v0.1.0..v0.1.1

<!-- generated by git-cliff -->
