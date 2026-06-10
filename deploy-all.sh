#!/bin/bash

set -e

KUBE_CONTEXT="$(kubectl config current-context)"
if [[ "$KUBE_CONTEXT" != *devnet* ]]; then
  echo "Refusing to deploy: current kube context '$KUBE_CONTEXT' does not contain 'devnet'." >&2
  echo "Switch to a devnet context with 'kubectl config use-context <ctx>' and retry." >&2
  exit 1
fi
echo "Using kube context: $KUBE_CONTEXT"

aws sso login

TAG="0.1.8"
IMAGE="public.ecr.aws/dlc-link/canton-decparty-manager"
DEPLOY_DIR="zarf/deployments/devnet"

echo "Building and pushing image with tag: $TAG"
docker build --no-cache --ssh default=$HOME/.ssh/github_docker -t dec-party-manager .
docker tag dec-party-manager "$IMAGE:$TAG"
docker logout public.ecr.aws
aws ecr-public get-login-password --region us-east-1 --profile "$AWS_PROFILE" | docker login --username AWS --password-stdin public.ecr.aws
docker push "$IMAGE:$TAG"

echo "Updating image tag in deployment configs..."
sed -i '' "s|$IMAGE:[^\"]*|$IMAGE:$TAG|g" "$DEPLOY_DIR/participant1/deployment.yaml"
sed -i '' "s|$IMAGE:[^\"]*|$IMAGE:$TAG|g" "$DEPLOY_DIR/participant2/deployment.yaml"
sed -i '' "s|$IMAGE:[^\"]*|$IMAGE:$TAG|g" "$DEPLOY_DIR/participant3/deployment.yaml"

echo "Deleting existing deployments (preserving PVCs)..."
kubectl delete deployment dec-party-manager-1 -n catalyst-canton --ignore-not-found
kubectl delete deployment dec-party-manager-2 -n catalyst-canton --ignore-not-found
kubectl delete deployment dec-party-manager-3 -n catalyst-canton --ignore-not-found

echo "Applying secrets..."
kubectl apply -f "$DEPLOY_DIR/participant1/secrets.yaml"
kubectl apply -f "$DEPLOY_DIR/participant2/secrets.yaml"
kubectl apply -f "$DEPLOY_DIR/participant3/secrets.yaml"

echo "Applying deployments..."
kubectl apply -f "$DEPLOY_DIR/participant1/deployment.yaml"
kubectl apply -f "$DEPLOY_DIR/participant2/deployment.yaml"
kubectl apply -f "$DEPLOY_DIR/participant3/deployment.yaml"

echo "Done! Deployed with tag: $TAG"
