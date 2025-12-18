#!/bin/bash

set -e

TAG="v0.0.8"
IMAGE="public.ecr.aws/dlc-link/canton-decparty-manager"
DEPLOY_DIR="zarf/deployments/devnet"

echo "Building and pushing image with tag: $TAG"
docker build --no-cache --ssh default=$HOME/.ssh/github_docker -t dec-party-manager .
docker tag dec-party-manager "$IMAGE:$TAG"
docker push "$IMAGE:$TAG"

echo "Updating image tag in deployment configs..."
sed -i '' "s|$IMAGE:[^\"]*|$IMAGE:$TAG|g" "$DEPLOY_DIR/participant1/deployment.yaml"
sed -i '' "s|$IMAGE:[^\"]*|$IMAGE:$TAG|g" "$DEPLOY_DIR/participant2/deployment.yaml"
sed -i '' "s|$IMAGE:[^\"]*|$IMAGE:$TAG|g" "$DEPLOY_DIR/participant3/deployment.yaml"

echo "Deleting existing deployments..."
kubectl delete -f "$DEPLOY_DIR/participant1/deployment.yaml" --ignore-not-found
kubectl delete -f "$DEPLOY_DIR/participant2/deployment.yaml" --ignore-not-found
kubectl delete -f "$DEPLOY_DIR/participant3/deployment.yaml" --ignore-not-found

echo "Applying deployments..."
kubectl apply -f "$DEPLOY_DIR/participant1/deployment.yaml"
kubectl apply -f "$DEPLOY_DIR/participant2/deployment.yaml"
kubectl apply -f "$DEPLOY_DIR/participant3/deployment.yaml"

echo "Done! Deployed with tag: $TAG"
