# Integration Guide

Step-by-step guide for deploying the Decentralized Party Manager (DPM) and integrating it with your Canton infrastructure.

## Prerequisites

### Canton Infrastructure

Each participant needs:

- **Canton participant node** with Admin API and Ledger API access
- The following **Admin API gRPC services** must be reachable:
  - `TopologyManagerReadService` (port 5002 by default)
  - `TopologyManagerWriteService`
  - `VaultManagerService`
  - `IdentityInitializationService`
  - `SynchronizerConnectivityService`
  - `PackageService`
- The following **Ledger API gRPC services** must be reachable:
  - `CommandService` (port 5001 by default)
  - `StateService`
  - `UserManagementService`
  - `PartyManagementService`
  - `InteractiveSubmissionService`
- Canton protocol version **34** or compatible

### Authentication (Production)

- **Keycloak instance** with a realm configured for your Canton network
- OAuth2 client with one of:
  - Client credentials (M2M) flow: `client_id` + `client_secret`
  - Password flow: `client_id` + `username` + `password`
- Ledger API user with `actAs` and `readAs` rights for the relevant parties

### Network

| Port | Protocol | Purpose |
|------|----------|---------|
| 8080 | TCP/HTTP | Web UI and REST API |
| 9000 | TCP/Noise | P2P communication between participants |

Both ports must be reachable between all participants. The Noise port (9000) carries encrypted traffic and does not need TLS termination.

### Software

- **Docker** (recommended) or Rust toolchain for building from source
- `cargo` 1.75+ if building from source

## Deployment

### Docker (Recommended)

```bash
docker run -d \
  --name dec-party-manager \
  -p 8080:8080 \
  -p 9000:9000 \
  -v $(pwd)/data:/app/data \
  -e DECPM_CANTON_ADMIN_HOST=canton-participant \
  -e DECPM_CANTON_ADMIN_PORT=5002 \
  -e DECPM_CANTON_LEDGER_HOST=canton-participant \
  -e DECPM_CANTON_LEDGER_PORT=5001 \
  -e DECPM_CANTON_SYNCHRONIZER=global \
  -e DECPM_CANTON_NETWORK=devnet \
  -e DECPM_LISTEN_ADDRESS=0.0.0.0 \
  -e DECPM_NOISE_PORT=9000 \
  -e DECPM_PUBLIC_ADDRESS=your-external-address \
  public.ecr.aws/dlc-link/canton-decparty-manager:latest
```

The container expects:
- `/app/data/` -- Persistent storage for the SQLite database (`decpm.db`), Noise keys, workflow data, and DAR files
- `DECPM_*` environment variables -- All node configuration (Canton endpoints, networking, timeouts)

Alternatively, place a `.env` file in the root directory (`/app/.env`) containing the `DECPM_*` variables and mount it as a volume:

```bash
docker run -d \
  --name dec-party-manager \
  -p 8080:8080 \
  -p 9000:9000 \
  -v $(pwd)/data:/app/data \
  -v $(pwd)/.env:/app/.env:ro \
  public.ecr.aws/dlc-link/canton-decparty-manager:latest
```

### Kubernetes

Full manifest with Secret, PVC, Deployment, and Service:

```yaml
apiVersion: v1
kind: Secret
metadata:
  name: dec-party-manager-secrets
type: Opaque
stringData:
  DECPM_CANTON_ADMIN_HOST: "canton-participant.default.svc.cluster.local"
  DECPM_CANTON_ADMIN_PORT: "5002"
  DECPM_CANTON_LEDGER_HOST: "canton-participant.default.svc.cluster.local"
  DECPM_CANTON_LEDGER_PORT: "5001"
  DECPM_CANTON_SYNCHRONIZER: "global"
  DECPM_CANTON_NETWORK: "devnet"
  DECPM_LISTEN_ADDRESS: "0.0.0.0"
  DECPM_NOISE_PORT: "9000"
  DECPM_PUBLIC_ADDRESS: "your-external-address"
  # Optional: Keycloak for frontend auth gating
  # DECPM_KEYCLOAK_URL: "https://keycloak.example.com"
  # DECPM_KEYCLOAK_REALM: "canton"
  # DECPM_KEYCLOAK_CLIENT_ID: "dpm-ui"
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: dec-party-manager-data
spec:
  accessModes:
    - ReadWriteOnce
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
        - name: init-data
          image: busybox:latest
          command: ["sh", "-c", "mkdir -p /app/data"]
          volumeMounts:
            - name: data
              mountPath: /app
      containers:
        - name: dec-party-manager
          image: public.ecr.aws/dlc-link/canton-decparty-manager:latest
          command:
            ["dec-party-manager", "-d", "/app", "serve",
             "--host", "0.0.0.0", "--port", "8080"]
          ports:
            - name: http
              containerPort: 8080
            - name: noise
              containerPort: 9000
          volumeMounts:
            - name: data
              mountPath: /app
          resources:
            requests:
              memory: "128Mi"
              cpu: "100m"
            limits:
              memory: "512Mi"
              cpu: "500m"
          envFrom:
            - secretRef:
                name: dec-party-manager-secrets
      volumes:
        - name: data
          persistentVolumeClaim:
            claimName: dec-party-manager-data
---
apiVersion: v1
kind: Service
metadata:
  name: dec-party-manager
spec:
  type: LoadBalancer
  ports:
    - name: http
      port: 80
      targetPort: 8080
    - name: noise
      port: 9000
      targetPort: 9000
  selector:
    app: dec-party-manager
```

### Binary

```bash
# Build from source
cargo build --release

# Option 1: Using environment variables
export DECPM_CANTON_ADMIN_HOST=localhost
export DECPM_CANTON_ADMIN_PORT=5002
export DECPM_CANTON_LEDGER_HOST=localhost
export DECPM_CANTON_LEDGER_PORT=5001
export DECPM_CANTON_SYNCHRONIZER=global
export DECPM_CANTON_NETWORK=devnet

./target/release/dec-party-manager -d /path/to/root-dir serve \
  --host 0.0.0.0 \
  --port 8080

# Option 2: Using a .env file in the root directory
cat > /path/to/root-dir/.env <<EOF
DECPM_CANTON_ADMIN_HOST=localhost
DECPM_CANTON_ADMIN_PORT=5002
DECPM_CANTON_LEDGER_HOST=localhost
DECPM_CANTON_LEDGER_PORT=5001
DECPM_CANTON_SYNCHRONIZER=global
DECPM_CANTON_NETWORK=devnet
EOF

./target/release/dec-party-manager -d /path/to/root-dir serve \
  --host 0.0.0.0 \
  --port 8080

# Option 3: Using CLI flags directly
./target/release/dec-party-manager -d /path/to/root-dir serve \
  --host 0.0.0.0 \
  --port 8080 \
  --canton-admin-host localhost \
  --canton-admin-port 5002 \
  --canton-ledger-host localhost \
  --canton-ledger-port 5001 \
  --canton-synchronizer global \
  --canton-network devnet
```

CLI options:

| Flag | Env Var | Default | Description |
|------|---------|---------|-------------|
| `-d`, `--dir` | -- | `.` | Root directory for persistent data; loads `.env` from this dir if present |
| `--host` | -- | `0.0.0.0` | HTTP server bind address |
| `--port` | -- | `8080` | HTTP server port |
| `--test` | -- | `false` | Enable test mode with mock authentication |
| `--db` | -- | `{dir}/data/decpm.db` | Path to SQLite database file |
| `--listen-address` | `DECPM_LISTEN_ADDRESS` | `0.0.0.0` | Noise server bind address |
| `--noise-port` | `DECPM_NOISE_PORT` | `9000` | Noise server port |
| `--public-address` | `DECPM_PUBLIC_ADDRESS` | (none) | External address for peers |
| `--canton-admin-host` | `DECPM_CANTON_ADMIN_HOST` | `127.0.0.1` | Canton Admin API host |
| `--canton-admin-port` | `DECPM_CANTON_ADMIN_PORT` | `5002` | Canton Admin API port |
| `--canton-ledger-host` | `DECPM_CANTON_LEDGER_HOST` | `127.0.0.1` | Canton Ledger API host |
| `--canton-ledger-port` | `DECPM_CANTON_LEDGER_PORT` | `5001` | Canton Ledger API port |
| `--canton-synchronizer` | `DECPM_CANTON_SYNCHRONIZER` | `global` | Synchronizer name |
| `--canton-network` | `DECPM_CANTON_NETWORK` | `devnet` | Canton network (`devnet`, `testnet`, `mainnet`) |
| `--keycloak-url` | `DECPM_KEYCLOAK_URL` | (none) | Keycloak server URL (frontend auth gating) |
| `--keycloak-realm` | `DECPM_KEYCLOAK_REALM` | (none) | Keycloak realm name (frontend auth gating) |
| `--keycloak-client-id` | `DECPM_KEYCLOAK_CLIENT_ID` | (none) | OAuth2 client ID (frontend auth gating) |
| `--timeout-handshake` | `DECPM_TIMEOUT_HANDSHAKE` | `30` | Noise handshake timeout (seconds) |
| `--timeout-message` | `DECPM_TIMEOUT_MESSAGE` | `120` | Noise message timeout (seconds) |
| `--timeout-retry-attempts` | `DECPM_TIMEOUT_RETRY_ATTEMPTS` | `3` | Connection retry count |
| `--timeout-retry-delay` | `DECPM_TIMEOUT_RETRY_DELAY` | `5` | Retry delay (seconds) |

**Configuration precedence** (highest to lowest):
1. CLI flags (`--canton-admin-host`)
2. Environment variables (`DECPM_CANTON_ADMIN_HOST`)
3. `.env` file in the `--dir` directory
4. Built-in defaults

## Configuration Reference

### Directory Structure

```
root-dir/
├── .env                   # Environment variables (optional, auto-loaded)
└── data/
    ├── decpm.db           # SQLite database (peers, party credentials)
    ├── noise.key          # Noise keypair (auto-generated)
    ├── dars/              # DAR files for contract deployment
    └── workflow-data/     # Per-workflow state directories
```

### Environment Variables

All node configuration is provided through environment variables (or their equivalent CLI flags). The `DECPM_*` prefix is used for all variables.

| Variable | CLI Flag | Type | Default | Description |
|----------|----------|------|---------|-------------|
| `DECPM_LISTEN_ADDRESS` | `--listen-address` | string | `0.0.0.0` | Noise server bind address |
| `DECPM_NOISE_PORT` | `--noise-port` | u16 | `9000` | Noise server port |
| `DECPM_PUBLIC_ADDRESS` | `--public-address` | string | (none) | External address peers use to reach this node. Falls back to listen address if unset |
| `DECPM_CANTON_ADMIN_HOST` | `--canton-admin-host` | string | `127.0.0.1` | Canton Admin API host |
| `DECPM_CANTON_ADMIN_PORT` | `--canton-admin-port` | u16 | `5002` | Canton Admin API port |
| `DECPM_CANTON_LEDGER_HOST` | `--canton-ledger-host` | string | `127.0.0.1` | Canton Ledger API host |
| `DECPM_CANTON_LEDGER_PORT` | `--canton-ledger-port` | u16 | `5001` | Canton Ledger API port |
| `DECPM_CANTON_SYNCHRONIZER` | `--canton-synchronizer` | string | `global` | Synchronizer name |
| `DECPM_CANTON_NETWORK` | `--canton-network` | string | `devnet` | Canton network environment (`devnet`, `testnet`, `mainnet`). Determines DSO API URL for AmuletRules queries |
| `DECPM_KEYCLOAK_URL` | `--keycloak-url` | string | (none) | Keycloak server URL for frontend auth gating |
| `DECPM_KEYCLOAK_REALM` | `--keycloak-realm` | string | (none) | Keycloak realm name for frontend auth gating |
| `DECPM_KEYCLOAK_CLIENT_ID` | `--keycloak-client-id` | string | (none) | OAuth2 client ID for frontend auth gating |
| `DECPM_TIMEOUT_HANDSHAKE` | `--timeout-handshake` | u64 | `30` | Noise handshake timeout in seconds |
| `DECPM_TIMEOUT_MESSAGE` | `--timeout-message` | u64 | `120` | Noise message timeout in seconds |
| `DECPM_TIMEOUT_RETRY_ATTEMPTS` | `--timeout-retry-attempts` | u32 | `3` | Max connection retries |
| `DECPM_TIMEOUT_RETRY_DELAY` | `--timeout-retry-delay` | u64 | `5` | Retry delay in seconds |

### `.env` File

A `.env` file placed in the `--dir` directory is automatically loaded before CLI/env parsing. This allows you to set all `DECPM_*` variables in a single file:

```env
DECPM_CANTON_ADMIN_HOST=canton-participant.default.svc.cluster.local
DECPM_CANTON_ADMIN_PORT=5002
DECPM_CANTON_LEDGER_HOST=canton-participant.default.svc.cluster.local
DECPM_CANTON_LEDGER_PORT=5001
DECPM_CANTON_SYNCHRONIZER=global
DECPM_CANTON_NETWORK=devnet
DECPM_LISTEN_ADDRESS=0.0.0.0
DECPM_NOISE_PORT=9000
DECPM_PUBLIC_ADDRESS=dpm.example.com
```

### SQLite Database (`decpm.db`)

The SQLite database stores:
- **Peers** -- Network peer list (participant ID, name, address, port, public key)
- **Party credentials** -- Per-party authentication credentials (dec party ID, member party ID, user ID, Keycloak config, package IDs)

The database is automatically created and migrated on startup. Its default location is `{dir}/data/decpm.db`, overridable with the `--db` flag.

## Authentication Setup

Party credentials (Keycloak authentication for each decentralized party) are configured at runtime via the `/party-config` API endpoint. They are stored in the SQLite database.

### Configuring Party Credentials via API

Use `PUT /party-config` to save or update credentials for a decentralized party:

```bash
curl -X PUT http://localhost:8080/party-config \
  -H "Content-Type: application/json" \
  -d '{
    "dec_party_id": "vault-network::1220abc...",
    "member_party_id": "member1::1220def...",
    "user_id": "service-user",
    "keycloak_url": "https://keycloak.example.com",
    "keycloak_realm": "canton",
    "keycloak_client_id": "dpm-service",
    "keycloak_client_secret": "your-client-secret",
    "packages": {}
  }'
```

Response:
```json
{ "success": true }
```

### M2M Flow (Client Credentials)

Best for automated/service deployments where no interactive user login is needed. Set `keycloak_client_secret` in the request body:

```bash
curl -X PUT http://localhost:8080/party-config \
  -H "Content-Type: application/json" \
  -d '{
    "dec_party_id": "vault-network::1220abc...",
    "member_party_id": "member1::1220def...",
    "user_id": "service-user",
    "keycloak_url": "https://keycloak.example.com",
    "keycloak_realm": "canton",
    "keycloak_client_id": "dpm-service",
    "keycloak_client_secret": "your-client-secret",
    "packages": {}
  }'
```

The application will automatically obtain and refresh tokens using the `client_credentials` grant type.

### Password Flow

For deployments where a specific user identity is needed. Set `keycloak_username` and `keycloak_password` in the request body:

```bash
curl -X PUT http://localhost:8080/party-config \
  -H "Content-Type: application/json" \
  -d '{
    "dec_party_id": "vault-network::1220abc...",
    "member_party_id": "member1::1220def...",
    "user_id": "alice",
    "keycloak_url": "https://keycloak.example.com",
    "keycloak_realm": "canton",
    "keycloak_client_id": "dpm-ui",
    "keycloak_username": "alice",
    "keycloak_password": "alice-password",
    "packages": {}
  }'
```

### Retrieving Party Configuration

Use `GET /party-config/{dec_party_id}` to retrieve the current configuration (secrets are masked):

```bash
curl http://localhost:8080/party-config/vault-network::1220abc...
```

Response:
```json
{
  "dec_party_id": "vault-network::1220abc...",
  "member_party_id": "member1::1220def...",
  "user_id": "service-user",
  "keycloak_url": "https://keycloak.example.com",
  "keycloak_realm": "canton",
  "keycloak_client_id": "dpm-service",
  "has_client_secret": true,
  "has_username": false,
  "has_password": false,
  "packages": {
    "governance_core": "#governance-core-v0-rc4",
    "governance_token_custody": "#governance-token-custody-v0-rc4",
    "utility_credential": "#utility-credential-app-v0",
    "utility_registry": "#utility-registry-app-v0",
    "vault": "#bitsafe-vault-v0-rc8",
    "vault_governance": "#bitsafe-vault-governance-v0-rc8"
  }
}
```

### Credential Update Semantics

When updating party credentials, secret fields (`keycloak_client_secret`, `keycloak_username`, `keycloak_password`) follow merge semantics:
- **Omitted** (`null` / not present) -- keep the existing value
- **Empty string** (`""`) -- clear the value
- **Non-empty string** -- set to the new value

This allows partial updates without re-submitting secrets.

### Test Mode

For development and testing without Keycloak:

```bash
dec-party-manager -d ./my-dir serve --test
```

Test mode uses a mock authentication provider that returns static tokens. All governance and ledger operations will use mock credentials.

### Verification

Check authentication status:
```bash
curl http://localhost:8080/auth/status
```

Response:
```json
{
  "parties": [
    {
      "dec_party_id": "vault-network::1220...",
      "member_party_id": "member1::1220...",
      "user_id": "alice",
      "keycloak_url": "https://keycloak.example.com",
      "keycloak_realm": "canton",
      "status": "authenticated",
      "rights": {
        "member_party_act_as": true,
        "member_party_read_as": true,
        "dec_party_act_as": true,
        "dec_party_read_as": true
      }
    }
  ]
}
```

Test authentication explicitly:
```bash
curl -X POST http://localhost:8080/auth/test
```

### JWT Requirements

The Ledger API user must match the JWT `sub` claim. The user needs:
- `actAs` rights for the `member_party_id` (local party)
- `readAs` rights for the `member_party_id`
- `actAs` rights for the `dec_party_id` (decentralized party)
- `readAs` rights for the `dec_party_id`

## Peer Network Setup

Peers are configured at runtime via the `/network-config` API endpoint and stored in the SQLite database.

### 1. Get Your Node's Public Key

After starting the server, retrieve your Noise public key:

```bash
curl http://localhost:8080/keys/status
```

Response:
```json
{
  "has_keys": true,
  "public_key": "03ab12cd34ef56..."
}
```

The key is auto-generated on first startup and stored in `data/noise.key`.

### 2. Exchange Peer Information

Each participant needs to share:
- **Participant ID** (from Canton: `GET /node-config`)
- **Name** (human-readable)
- **Address** (hostname or IP reachable by other participants)
- **Port** (Noise port, default 9000)
- **Public Key** (from `GET /keys/status`)

### 3. Add Peers

Via the UI (Network Config page) or via the API. Use `POST /network-config` with an array of peer objects:

```bash
curl -X POST http://localhost:8080/network-config \
  -H "Content-Type: application/json" \
  -d '[
    {
      "participant_id": "node1::1220abc...",
      "name": "Node 1",
      "address": "10.0.0.1",
      "port": 9000,
      "public_key": "03ab12cd..."
    },
    {
      "participant_id": "node2::1220def...",
      "name": "Node 2",
      "address": "10.0.0.2",
      "port": 9000,
      "public_key": "02ef34ab..."
    }
  ]'
```

Response:
```json
{ "success": true }
```

Retrieve the current peer list:
```bash
curl http://localhost:8080/network-config
```

Response:
```json
{
  "peers": [
    {
      "participant_id": "node1::1220abc...",
      "name": "Node 1",
      "address": "10.0.0.1",
      "port": 9000,
      "public_key": "03ab12cd...",
      "party": null
    }
  ]
}
```

### 4. Verify Connectivity

```bash
curl http://localhost:8080/participants-status
```

Response:
```json
{
  "statuses": [
    { "id": "node1::1220abc...", "status": "CurrentNode" },
    { "id": "node2::1220def...", "status": "Connected" },
    { "id": "node3::1220ghi...", "status": "Unreachable" }
  ]
}
```

Status values:
- `CurrentNode` -- This node (always shown)
- `Connected` -- Noise handshake succeeded
- `Unreachable` -- TCP connection failed
- `HandshakeFailed` -- TCP connected but Noise handshake/decryption failed (likely wrong public key)

## Creating Your First Decentralized Party

### Prerequisites

- All participants running and configured with peers via `POST /network-config`
- All peers showing `Connected` status
- At least 2 participants

### Step 1: Start Onboarding (Coordinator)

From the coordinator node:

```bash
curl -X POST http://localhost:8080/onboarding \
  -H "Content-Type: application/json" \
  -d '{
    "party_id_prefix": "my-vault-network",
    "peer_ids": [
      "node2::1220def...",
      "node3::1220ghi..."
    ]
  }'
```

Response:
```json
{
  "status": "inprogress",
  "message": "Onboarding workflow started"
}
```

### Step 2: Accept Invitations (Attestors)

On each attestor node, invitations appear in the UI or via API:

```bash
# Check for pending invitations
curl http://localhost:8080/invitations
```

```json
{
  "invitations": [
    {
      "id": "onboarding-03ab12cd34ef",
      "invitation_type": "Onboarding",
      "coordinator_pubkey": "03ab12cd34ef...",
      "coordinator_name": null,
      "received_at": 1737000000
    }
  ]
}
```

Accept the invitation:
```bash
curl -X POST http://localhost:8080/invitations/accept \
  -H "Content-Type: application/json" \
  -d '{ "id": "onboarding-03ab12cd34ef" }'
```

### Step 3: Monitor Progress

Poll the status endpoint on the coordinator:

```bash
curl http://localhost:8080/onboarding/status
```

```json
{
  "status": "inprogress",
  "error": null
}
```

Status values: `idle`, `inprogress`, `completed`, `failed`.

### Step 4: Verify the Party

After completion, verify the new decentralized party:

```bash
curl "http://localhost:8080/decentralized-parties?prefix=my-vault-network"
```

```json
{
  "parties": [
    {
      "party_id": "my-vault-network::1220abc...",
      "threshold": 2,
      "owners": ["1220aaa...", "1220bbb...", "1220ccc..."],
      "my_owner_key": "1220aaa...",
      "participants": [
        { "participant_uid": "node1::1220...", "permission": "submission" },
        { "participant_uid": "node2::1220...", "permission": "submission" },
        { "participant_uid": "node3::1220...", "permission": "submission" }
      ],
      "contracts": []
    }
  ]
}
```

## Deploying Governance Contracts

### Step 1: Prepare DAR Files

Encode your DAR files as base64:

```bash
base64 -i governance-core.dar -o governance-core.b64
base64 -i governance-token-custody.dar -o governance-token-custody.b64
```

### Step 2: Define Contracts

Contract definitions specify the Daml templates to instantiate. Each field uses a typed definition:

| Field Type | Description |
|------------|-------------|
| `decentralized_party` | The decentralized party ID |
| `operator_party` | The operator party ID |
| `participant_party` | A specific party ID (`{ "type": "participant_party", "id": "..." }`) |
| `text` | Static text (`{ "type": "text", "value": "..." }`) |
| `int64` | Integer (`{ "type": "int64", "value": 42 }`) |
| `bool` | Boolean (`{ "type": "bool", "value": true }`) |
| `instrument` | Instrument record (`{ "type": "instrument", "id": "..." }`) |
| `attestors_set` | Set of all participant parties |
| `party_set` | Set of specific parties |
| `rel_time` | Relative time in microseconds |
| `optional` | Optional wrapper (`{ "type": "optional", "inner": { ... } }`) |
| `record` | Nested record |
| `governance_threshold` | Governance threshold value |

### Step 3: Start Contracts Workflow

**GovernanceRules (recommended for new deployments):**

```bash
curl -X POST http://localhost:8080/contracts \
  -H "Content-Type: application/json" \
  -d '{
    "decentralized_party_id": "my-vault-network::1220abc...",
    "participant_ids": [
      "node1::1220...",
      "node2::1220...",
      "node3::1220..."
    ],
    "participant_parties": [
      "member1::1220...",
      "member2::1220...",
      "member3::1220..."
    ],
    "operator_party": "operator::1220...",
    "dar_files": [
      {
        "filename": "governance-core.dar",
        "data": "<base64-encoded-dar>"
      },
      {
        "filename": "governance-token-custody.dar",
        "data": "<base64-encoded-dar>"
      }
    ],
    "contracts": [
      {
        "id": "governance-rules",
        "name": "GovernanceRules",
        "package_id": "#governance-core-v0-rc4",
        "module_name": "Governance.Rules",
        "entity_name": "GovernanceRules",
        "fields": [
          { "type": "decentralized_party" },
          { "type": "attestors_set" },
          { "type": "governance_threshold" },
          { "type": "rel_time", "microseconds": 86400000000 }
        ]
      }
    ]
  }'
```

**VaultGovernanceRules (legacy, for existing vault deployments):**

```bash
curl -X POST http://localhost:8080/contracts \
  -H "Content-Type: application/json" \
  -d '{
    "decentralized_party_id": "my-vault-network::1220abc...",
    "participant_ids": [
      "node1::1220...",
      "node2::1220...",
      "node3::1220..."
    ],
    "participant_parties": [
      "member1::1220...",
      "member2::1220...",
      "member3::1220..."
    ],
    "operator_party": "operator::1220...",
    "dar_files": [
      {
        "filename": "vault-governance.dar",
        "data": "<base64-encoded-dar>"
      }
    ],
    "contracts": [
      {
        "id": "vault-governance-rules",
        "name": "VaultGovernanceRules",
        "package_id": "#bitsafe-vault-governance-v0-rc8",
        "module_name": "BitsafeVault.VaultGovernance",
        "entity_name": "VaultGovernanceRules",
        "fields": [
          { "type": "decentralized_party" },
          { "type": "attestors_set" },
          { "type": "governance_threshold" },
          { "type": "optional", "inner": { "type": "rel_time", "microseconds": 86400000000 } }
        ]
      }
    ]
  }'
```

The workflow requires at least **3 participants** and follows the same invitation/acceptance flow as onboarding.

## Connecting to Daml Applications

After creating a decentralized party and deploying governance contracts, the party can be used in Daml applications:

- Use the **decentralized party ID** (e.g., `my-vault-network::1220...`) as the `actAs` party in Daml commands
- Each member's local `member_party_id` must have `actAs`/`readAs` rights for both itself and the decentralized party
- **GovernanceRules** controls self-management (member changes, threshold) and domain actions (token transfers, voting, preapprovals) via the `GovernableAction` interface
- **VaultGovernanceRules** (legacy) controls vault lifecycle operations (deploy, pause, limits) via its built-in action enum
- Topology workflows control participant membership (onboarding, kick)

## Roles and Permissions

### Canton Participant Permissions

Participants in a decentralized party can have different permission levels:

| Permission | Description |
|------------|-------------|
| `Submission` | Can submit transactions (default for all members) |
| `Confirmation` | Can confirm transactions (mediator role) |
| `Observation` | Read-only access to party's transactions |

### Coordinator vs Attestor

These are **workflow roles**, not permanent privileges:
- Any participant can act as coordinator for any workflow
- The coordinator role is determined at workflow initiation time
- There is no privilege difference between coordinator and attestor nodes
- The coordinator simply manages the workflow orchestration; all participants sign equally

### Ledger API User Rights

Each participant's Ledger API user needs:

| Right | Party | Purpose |
|-------|-------|---------|
| `actAs` | member_party_id | Submit commands as the local member |
| `readAs` | member_party_id | Read contracts visible to the member |
| `actAs` | dec_party_id | Submit commands as the decentralized party |
| `readAs` | dec_party_id | Read contracts visible to the decentralized party |

## API Reference

### Configuration

| Method | Endpoint | Description | Request Body | Response |
|--------|----------|-------------|--------------|----------|
| GET | `/node-config` | Get node configuration | -- | Node config JSON |
| GET | `/network-config` | Get peer list from database | -- | `{ "peers": [...] }` |
| POST | `/network-config` | Save peer list to database | `[{ "participant_id": "...", "name": "...", "address": "...", "port": 9000, "public_key": "..." }]` | `{ "success": true }` |
| GET | `/party-config/{dec_party_id}` | Get party config (secrets masked) | -- | Party config JSON |
| PUT | `/party-config` | Save/update party credentials | Party config JSON | `{ "success": true }` |
| POST | `/party-config/discover-member-party` | Discover a member party's Canton ID via Keycloak (admin-only) | `{ keycloak_url, keycloak_realm, keycloak_client_id, keycloak_client_secret? \| keycloak_username + keycloak_password }` | `DiscoverMemberPartyResponse` |

### Keys

| Method | Endpoint | Description | Response |
|--------|----------|-------------|----------|
| GET | `/keys/status` | Get Noise keypair status | `{ "has_keys": bool, "public_key": "hex..." }` |

### Parties

| Method | Endpoint | Description | Query Params | Response |
|--------|----------|-------------|--------------|----------|
| GET | `/decentralized-parties` | List decentralized parties | `prefix` (optional) | `{ "parties": [...] }` |
| GET | `/participants-status` | Peer connectivity status | -- | `{ "statuses": [...] }` |
| GET | `/packages/compare-peers` | Cross-check vetted-package IDs across peers (admin-only) | -- | `{ "missing_on": [...], "extra_on": [...] }` |

### Workflows

| Method | Endpoint | Description | Request Body |
|--------|----------|-------------|--------------|
| POST | `/onboarding` | Start onboarding | `{ "party_id_prefix": "...", "peer_ids": [...] }` |
| GET | `/onboarding/status` | Get onboarding progress | -- |
| POST | `/kick` | Start kick workflow | `{ "decentralized_party_id": "...", "participant_id": "...", "namespace_fingerprint": "...", "new_threshold": N }` |
| GET | `/kick/status` | Get kick progress | -- |
| POST | `/contracts` | Start contracts workflow | `{ "decentralized_party_id": "...", "participant_ids": [...], ... }` |
| GET | `/contracts/status` | Get contracts progress | -- |
| POST | `/dars/upload` | Upload DARs to current node only | `{ "dar_files": [{ "filename": "...", "data": "<base64>" }] }` |
| POST | `/dars/distribute` | Distribute DARs to all participants | `{ "dar_files": [{ "filename": "...", "data": "<base64>" }] }` |
| GET | `/dars/distribute/status` | Get DARs distribution progress | -- |
| GET | `/packages/vetted` | Get packages uploaded on this node | -- |

### Invitations

| Method | Endpoint | Description | Request Body |
|--------|----------|-------------|--------------|
| GET | `/invitations` | List pending invitations | -- |
| POST | `/invitations/accept` | Accept an invitation | `{ "id": "..." }` |
| POST | `/invitations/decline` | Decline an invitation | `{ "id": "..." }` |

### Authentication

| Method | Endpoint | Description | Response |
|--------|----------|-------------|----------|
| GET | `/auth-config` | Get the configured auth provider (mock or keycloak) | `{ "provider": "..." }` |
| GET | `/auth/status` | Get auth status for all parties | `{ "parties": [...] }` |
| POST | `/auth/test` | Test authentication | `{ "results": [...] }` |
| POST | `/auth/grant-rights` | Grant Canton actAs/readAs rights to a party (admin-only) | `{ "rights": { ... } }` |

### Governance

| Method | Endpoint | Description | Query/Body |
|--------|----------|-------------|------------|
| GET | `/governance/confirmations` | List confirmations grouped by action | `?party_id=...` |
| GET | `/governance/state` | Get governance contract state | `?party_id=...` |
| POST | `/governance/propose` | Create a domain action proposal | `ProposeActionRequest` (see below) |
| POST | `/governance/confirm` | Submit a governance confirmation | `ConfirmActionRequest` (see below) |
| POST | `/governance/execute` | Execute a confirmed action | `ExecuteActionRequest` (see below) |
| POST | `/governance/expire` | Expire stale confirmation | `ExpireConfirmationRequest` (see below) |
| POST | `/governance/cancel` | Cancel (revoke) own confirmation | `CancelConfirmationRequest` (see below) |

#### Governance Types

All governance mutation endpoints accept a `governance_type` field that selects which governance system to use:

| Value | Description |
|-------|-------------|
| `"vault"` | Legacy `VaultGovernanceRules` (default if omitted) |
| `"core_self"` | `GovernanceRules` self-management actions (add/remove member, set threshold/timeout) |
| `"core_domain"` | `GovernanceRules` domain actions via `GovernableAction` proposals |

#### Propose (Domain Actions)

`POST /governance/propose` creates a domain action proposal and auto-confirms it as the proposer. Only used with `GovernanceRules` (not `VaultGovernanceRules`).

```json
{
  "party_id": "my-vault-network::1220abc...",
  "rules_contract_id": "<governance-rules-cid>",
  "proposal": {
    "type": "generic_vote",
    "description": "We should switch to dark theme for our website"
  }
}
```

The server populates the `proposer` field on the proposal contract automatically (using the calling party's identity). As of `v0-rc4`, that proposer must be a member of the targeted `GovernanceRules` or appear in its `additionalProposers` allowlist; otherwise the auto-confirmation step rejects the proposal at confirm time.

Available proposal types:

| Type | Fields | Description |
|------|--------|-------------|
| `generic_vote` | `description` | Free-text governance vote |
| `setup_cc_preapproval` | `provider`, `expected_dso` | Set up Canton Coin transfer preapproval |
| `setup_token_preapproval` | `operator`, `instrument_admin`, `instrument_allowances` (optional) | Set up utility token transfer preapproval |
| `transfer` | `transfer_factory_cid`, `expected_admin`, `receiver`, `amount`, `instrument_id`, `input_holding_cids` (optional) | Transfer tokens from governance party |
| `accept_transfer` | `transfer_instruction_cid` | Accept an incoming token transfer |

#### Confirm

`POST /governance/confirm` submits a confirmation for an action or proposal.

**For vault governance (`governance_type: "vault"` or omitted):**

```json
{
  "party_id": "my-vault-network::1220abc...",
  "rules_contract_id": "<vault-governance-rules-cid>",
  "action": { "type": "vault_pause", "vault_id": "<vault-cid>" }
}
```

**For core self-management (`governance_type: "core_self"`):**

```json
{
  "party_id": "my-vault-network::1220abc...",
  "rules_contract_id": "<governance-rules-cid>",
  "action": { "type": "governance_set_threshold", "new_threshold": 3 },
  "governance_type": "core_self"
}
```

Available `core_self` action types (DAML `GovernanceSelfAction` variants):

| `type` | Required fields | DAML variant |
|---|---|---|
| `governance_add_member` | `member`, `new_threshold` | `SelfAction_AddMemberAndSetThreshold` |
| `governance_remove_member` | `member`, `new_threshold` | `SelfAction_RemoveMemberAndSetThreshold` |
| `governance_set_threshold` | `new_threshold` | `SelfAction_SetThreshold` |
| `governance_set_timeout` | `new_timeout_microseconds` | `SelfAction_SetTimeout` |
| `governance_add_additional_proposer` | `additional_proposer` (party id) | `SelfAction_AddAdditionalProposer` |
| `governance_remove_additional_proposer` | `additional_proposer` (party id) | `SelfAction_RemoveAdditionalProposer` |

The two `*_additional_proposer` variants (added in `v0-rc4`) mutate the `additionalProposers` allowlist on `GovernanceRules`. See ARCHITECTURE.md for the proposer-authorization model.

**For domain actions (`governance_type: "core_domain"`):**

```json
{
  "party_id": "my-vault-network::1220abc...",
  "rules_contract_id": "<governance-rules-cid>",
  "action": { "type": "governance_set_threshold", "new_threshold": 0 },
  "governance_type": "core_domain",
  "proposal_cid": "<proposal-contract-id>"
}
```

Note: For `core_domain`, the `action` field is not used for matching -- the `proposal_cid` identifies the proposal. The `action` field is still required but can contain a placeholder value.

#### Execute

`POST /governance/execute` executes an action once threshold confirmations are met.

```json
{
  "party_id": "my-vault-network::1220abc...",
  "rules_contract_id": "<governance-rules-cid>",
  "action": { "type": "governance_set_threshold", "new_threshold": 0 },
  "confirmation_cids": ["<confirmation-cid-1>", "<confirmation-cid-2>"],
  "disclosed_contracts": [],
  "governance_type": "core_domain",
  "proposal_cid": "<proposal-contract-id>"
}
```

The `disclosed_contracts` field is used for token custody operations where the counterparty's contracts must be disclosed for execution.

#### Expire

`POST /governance/expire` removes a stale confirmation that has passed its expiry time.

```json
{
  "party_id": "my-vault-network::1220abc...",
  "rules_contract_id": "<governance-rules-cid>",
  "confirmation_cid": "<stale-confirmation-cid>",
  "governance_type": "core_self"
}
```

#### Cancel

`POST /governance/cancel` allows a member to revoke their own confirmation.

```json
{
  "party_id": "my-vault-network::1220abc...",
  "confirmation_cid": "<my-confirmation-cid>",
  "governance_type": "core_domain"
}
```

### Contracts and Services

| Method | Endpoint | Description | Query Params |
|--------|----------|-------------|--------------|
| GET | `/vaults` | List deployed Vault contracts | `?party_id=...` |
| GET | `/services/provider` | List ProviderService contracts | `?party_id=...` |
| GET | `/services/user` | List UserService contracts | `?party_id=...` |
| GET | `/services/registrar` | List RegistrarService contracts | `?party_id=...` |
| GET | `/contracts/query` | Query active contracts by template or interface | `?party_id=...&package_id=...&module_name=...&entity_name=...&interface=false` |
| GET | `/packages` | Get configured package IDs for a party | `?party_id=...` |
| GET | `/network-info` | Get DSO party ID and AmuletRules contract | -- |
| POST | `/token-standard-contracts` | CORS proxy to devnet token standard contracts endpoint | JSON body (forwarded as-is) |

### Response Formats

**Workflow status response:**
```json
{
  "status": "idle | inprogress | completed | failed",
  "error": "Error message if failed, null otherwise"
}
```

**Governance confirmations response:**

The response includes two arrays: `actions` for value-matched actions (vault governance and core self-management) and `domain_actions` for proposal-matched domain actions.

```json
{
  "actions": [
    {
      "action_hash": "sha256-of-serialized-action",
      "action": { "type": "vault_pause", "vault_id": "..." },
      "confirmations": [
        {
          "contract_id": "confirmation-cid",
          "action": { "type": "vault_pause", "vault_id": "..." },
          "confirming_party": "member1::1220..."
        }
      ],
      "confirmation_count": 1,
      "can_execute": false
    }
  ],
  "domain_actions": [
    {
      "proposal_cid": "00abc123...",
      "action_label": "GenericVote",
      "description": "We should switch to dark theme for our website",
      "confirmations": [
        {
          "contract_id": "confirmation-cid",
          "action": { "type": "governance_set_threshold", "new_threshold": 0 },
          "confirming_party": "member1::1220..."
        }
      ],
      "confirmation_count": 1,
      "can_execute": false
    }
  ],
  "threshold": 2,
  "member_party_id": "member1::1220..."
}
```

**Vault info response:**
```json
{
  "vaults": [
    {
      "contract_id": "...",
      "vault_name": "BTC Vault",
      "share_symbol": "vBTC",
      "is_paused": false,
      "vault_manager": "vault-network::1220..."
    }
  ]
}
```
