#!/usr/bin/env bash
set -euox pipefail

docker build -t d-k8s-example-shared-storage ./app -f Dockerfile
k8s push d-k8s-example-shared-storage --name example-shared-storage
k8s k -- apply -f resources.yaml
k8s k -- rollout restart statefulset/app
k8s k -- wait --for condition=Ready  pod/app-2
xdg-open http://localhost:8001/api/v1/namespaces/default/services/app/proxy/
k8s k -- proxy --port 8001