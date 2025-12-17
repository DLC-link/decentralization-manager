# Canton Decentralized Party Manager - User Guide

## Quick Start with Docker

```bash
docker run -d \
  --name dec-party-manager \
  -p 8080:8080 \
  -p 9000:9000 \
  -v $(pwd)/config:/config \
  -v $(pwd)/data:/data \
  public.ecr.aws/dlc-link/canton-decparty-manager:v0.0.7
```

Access the web UI at `http://localhost:8080`

## Configuration

Before running, create a `config/node.toml` file:

```toml
[node]
node_id = "my-participant"
listen_address = "0.0.0.0"
port = 9000

[canton]
admin_api_host = "your-canton-node"
admin_api_port = 5002
ledger_api_host = "your-canton-node"
ledger_api_port = 5001
ledger_api_user_id = "ledger-api-user"
synchronizer = "global"
# ledger_api_token = "your-jwt-token"  # Optional

[timeouts]
handshake_timeout_secs = 30
message_timeout_secs = 120
connection_retry_attempts = 3
connection_retry_delay_secs = 5
```

## Port Requirements

| Port | Purpose |
|------|---------|
| 8080 | Web UI and API |
| 9000 | P2P communication between participants |

Ensure both ports are accessible. For P2P to work, port 9000 must be reachable by other participants.

## Kubernetes Deployment

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: dec-party-manager-config
data:
  node.toml: |
    [node]
    node_id = "my-participant"
    listen_address = "0.0.0.0"
    port = 9000

    [canton]
    admin_api_host = "canton-node"
    admin_api_port = 5002
    ledger_api_host = "canton-node"
    ledger_api_port = 5001
    ledger_api_user_id = "ledger-api-user"
    synchronizer = "global"

    [timeouts]
    handshake_timeout_secs = 30
    message_timeout_secs = 120
    connection_retry_attempts = 3
    connection_retry_delay_secs = 5
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: dec-party-manager-data
spec:
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 1Gi
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: dec-party-manager
spec:
  replicas: 1
  selector:
    matchLabels:
      app: dec-party-manager
  template:
    metadata:
      labels:
        app: dec-party-manager
    spec:
      initContainers:
        - name: copy-config
          image: busybox:latest
          command: ['sh', '-c', 'cp /config-source/* /config/']
          volumeMounts:
            - name: config-source
              mountPath: /config-source
            - name: data
              mountPath: /config
              subPath: config
      containers:
        - name: dec-party-manager
          image: public.ecr.aws/dlc-link/canton-decparty-manager:latest
          ports:
            - containerPort: 8080
            - containerPort: 9000
          volumeMounts:
            - name: data
              mountPath: /config
              subPath: config
            - name: data
              mountPath: /data
              subPath: data
      volumes:
        - name: config-source
          configMap:
            name: dec-party-manager-config
        - name: data
          persistentVolumeClaim:
            claimName: dec-party-manager-data
---
apiVersion: v1
kind: Service
metadata:
  name: dec-party-manager
spec:
  type: ClusterIP
  ports:
    - port: 80
      targetPort: 8080
      name: http
    - port: 9000
      targetPort: 9000
      name: p2p
  selector:
    app: dec-party-manager
```

Apply with:

```bash
kubectl apply -f dec-party-manager.yaml
```

## Adding Peers

1. Open the web UI
2. Expand **Network Configuration**
3. Click **Add Peer**
4. Paste a CSV row shared by the other participant - the fields will auto-fill:
   ```
   participant-2,Participant 2,10.0.0.2,9000,03ab12cd...
   ```
5. Alternatively, fill in the fields manually:
   - **ID**: Peer's node ID
   - **Name**: Display name
   - **Address**: IP or hostname
   - **Port**: P2P port (usually 9000)
   - **Public Key**: Peer's public key (found in their UI under Node Configuration)
6. Click **Save**

## Removing a Participant (Kick)

1. Open the web UI
2. Select the party
3. Click **Remove Participant**
4. Select the participant to remove
5. Confirm

Requires majority approval from remaining participants.

## Troubleshooting

**Peer not connecting:**
- Verify the peer's address, port, and public key are correct
- Ensure port 9000 is open on both sides

**Canton connection failed:**
- Check `admin_api_host` and `ledger_api_host` in `node.toml`
- Verify Canton node is running and accessible

**View logs:**
```bash
# Docker
docker logs dec-party-manager

# Kubernetes
kubectl logs deployment/dec-party-manager
```
