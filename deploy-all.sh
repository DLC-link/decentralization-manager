#!/bin/bash

set -e

aws sso login

TAG="0.0.31"
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
