#!/bin/bash

basedir=$(dirname "${0}")

docker build -t git-events-runner:local -f "${basedir}"/git-events-runner.dockerfile "${basedir}"/.. && \
docker build -t gitrepo-cloner:local -f "${basedir}"/gitrepo-cloner.dockerfile "${basedir}"/..
docker build -t action-worker:local -f "${basedir}"/action-worker.dockerfile "${basedir}"/..
