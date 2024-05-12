#!/usr/bin/env bash
set -euo pipefail

BASE_DIR=$( cd -- "$( dirname -- "${BASH_SOURCE[0]}" )" &> /dev/null && pwd )
CHART_DIR="${BASE_DIR}/git-events-runner"
BUNDLE_FILE=${BASE_DIR}/crds.yaml

# generate CRD bundle from the latest binary
APP_VERSION=${LOCAL_APP_VERSION:-$(yq .appVersion ${CHART_DIR}/Chart.yaml)}
APP_IMAGE=${LOCAL_APP_IMAGE:-$(yq .image.repository ${CHART_DIR}/values.yaml)}

docker run --rm ${APP_IMAGE}:${APP_VERSION} crds > ${BUNDLE_FILE}

# Split the generated bundle yaml file to inject control flags
mkdir -p ${CHART_DIR}/templates/crds/
yq e -Ns "\"${CHART_DIR}/templates/crds/\" + .spec.names.singular" ${BUNDLE_FILE}

# Add helm if statement for controlling the install of CRDs
for i in "${CHART_DIR}"/templates/crds/*.yml; do
  cp "$i" "$i.bkp"
  echo "{{- if .Values.installCRDs }}" > "$i"
  cat "$i.bkp" >> "$i"
  echo "{{- end }}" >> "$i"
  rm "$i.bkp"
  mv "$i" "${i%.yml}.yaml"
done

rm ${BUNDLE_FILE}
