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
  -v $(pwd)/config:/config \
  -v $(pwd)/data:/data \
  public.ecr.aws/dlc-link/canton-decparty-manager:latest
```

The container expects:
- `/config/node.toml` -- Node configuration
- `/config/peers.csv` -- Peer list (auto-created if missing)
- `/data/` -- Persistent storage for keys, workflow data, and DAR files

### Kubernetes

Full manifest with ConfigMap, PVC, Deployment, and Service:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: dec-party-manager-config
data:
  node.toml: |
    [node]
    listen_address = "0.0.0.0"
    public_address = "your-external-address"
    port = 9000

    [canton]
    admin_api_host = "canton-participant.default.svc.cluster.local"
    admin_api_port = 5002
    ledger_api_host = "canton-participant.default.svc.cluster.local"
    ledger_api_port = 5001
    synchronizer = "global"
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
        - name: copy-config
          image: busybox:latest
          command:
            - sh
            - -c
            - |
              mkdir -p /app/config /app/data
              cp /config-readonly/* /app/config/
          volumeMounts:
            - name: config
              mountPath: /config-readonly
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
      volumes:
        - name: config
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

# Run
./target/release/dec-party-manager -d /path/to/root-dir serve \
  --host 0.0.0.0 \
  --port 8080
```

CLI options:

| Flag | Default | Description |
|------|---------|-------------|
| `-d`, `--dir` | `.` | Root directory containing `config/` and `data/` |
| `--host` | `0.0.0.0` | HTTP server bind address |
| `--port` | `8080` | HTTP server port |
| `--test` | `false` | Enable test mode with mock authentication |

## Configuration Reference

### Directory Structure

```
root-dir/
├── config/
│   ├── node.toml          # Node configuration
│   └── peers.csv          # Network peer list (auto-created)
└── data/
    ├── noise.key          # Noise keypair (auto-generated)
    ├── dars/              # DAR files for contract deployment
    └── workflow-data/     # Per-workflow state directories
```

### Node Configuration (`config/node.toml`)

```toml
[node]
# Canton participant UID. If omitted, auto-resolved from Canton Admin API on startup.
# participant_id = "participant1::1220abc..."
listen_address = "0.0.0.0"          # Noise listen address
port = 9000                          # Noise listen port (default: 9000)
# Public address peers use to reach this node. Falls back to listen_address if unset.
# public_address = "dpm.example.com"

[canton]
admin_api_host = "localhost"         # Canton Admin API host
admin_api_port = 5002                # Canton Admin API port
ledger_api_host = "localhost"        # Canton Ledger API host
ledger_api_port = 5001               # Canton Ledger API port
synchronizer = "global"              # Synchronizer name (default: "global")

[timeouts]
handshake_timeout_secs = 30          # Noise handshake timeout (default: 30)
message_timeout_secs = 120           # Noise message timeout (default: 120)
connection_retry_attempts = 3        # Connection retry count (default: 3)
connection_retry_delay_secs = 5      # Delay between retries (default: 5)

# Per-party authentication credentials (one [[parties]] block per decentralized party)
[[parties]]
dec_party_id = "vault-network::1220abc..."      # Decentralized party ID
member_party_id = "member1::1220def..."          # Local member party ID
user_id = "user-123"                             # Ledger API user (must match JWT 'sub')

[parties.keycloak]
url = "https://keycloak.example.com"
realm = "canton"
client_id = "dpm-client"
# M2M flow: set client_secret
client_secret = "your-secret"
# OR password flow: set username + password
# username = "admin"
# password = "admin-password"
```

**Field reference:**

| Section | Field | Type | Default | Description |
|---------|-------|------|---------|-------------|
| `[node]` | `participant_id` | string | (auto) | Canton participant UID |
| `[node]` | `listen_address` | string | `0.0.0.0` | Noise server bind address |
| `[node]` | `port` | u16 | `9000` | Noise server port |
| `[node]` | `public_address` | string | (none) | External address for peers |
| `[canton]` | `admin_api_host` | string | required | Canton Admin API host |
| `[canton]` | `admin_api_port` | u16 | required | Canton Admin API port |
| `[canton]` | `ledger_api_host` | string | required | Canton Ledger API host |
| `[canton]` | `ledger_api_port` | u16 | required | Canton Ledger API port |
| `[canton]` | `synchronizer` | string | `global` | Synchronizer name |
| `[timeouts]` | `handshake_timeout_secs` | u64 | `30` | Noise handshake timeout |
| `[timeouts]` | `message_timeout_secs` | u64 | `120` | Noise message timeout |
| `[timeouts]` | `connection_retry_attempts` | u32 | `3` | Max connection retries |
| `[timeouts]` | `connection_retry_delay_secs` | u64 | `5` | Retry delay in seconds |
| `[[parties]]` | `dec_party_id` | CantonId | required | Decentralized party ID |
| `[[parties]]` | `member_party_id` | CantonId | required | Local member party ID |
| `[[parties]]` | `user_id` | string | required | Ledger API user ID |
| `[parties.keycloak]` | `url` | string | required | Keycloak server URL |
| `[parties.keycloak]` | `realm` | string | required | Keycloak realm name |
| `[parties.keycloak]` | `client_id` | string | required | OAuth2 client ID |
| `[parties.keycloak]` | `client_secret` | string | (none) | Client secret (M2M flow) |
| `[parties.keycloak]` | `username` | string | (none) | Username (password flow) |
| `[parties.keycloak]` | `password` | string | (none) | Password (password flow) |

### Peers Configuration (`config/peers.csv`)

CSV format with header row:

```csv
participant_id,name,address,port,public_key,party
PAR::node1::1220abc...,Node 1,10.0.0.1,9000,03ab12cd...,
PAR::node2::1220def...,Node 2,10.0.0.2,9000,02ef34ab...,
SV::node3::1220ghi...,Node 3,10.0.0.3,9000,03cd56ef...,
```

| Column | Description |
|--------|-------------|
| `participant_id` | Canton participant UID (e.g., `PAR::node1::1220...`) |
| `name` | Human-readable display name |
| `address` | Hostname or IP for Noise connections |
| `port` | Noise protocol port |
| `public_key` | Hex-encoded secp256k1 compressed public key (33 bytes) |
| `party` | Canton party ID (populated after onboarding, can be left empty) |

## Authentication Setup

### M2M Flow (Client Credentials)

Best for automated/service deployments where no interactive user login is needed.

In `node.toml`:
```toml
[[parties]]
dec_party_id = "vault-network::1220..."
member_party_id = "member1::1220..."
user_id = "service-user"

[parties.keycloak]
url = "https://keycloak.example.com"
realm = "canton"
client_id = "dpm-service"
client_secret = "your-client-secret"
```

The application will automatically obtain and refresh tokens using the `client_credentials` grant type.

### Password Flow

For deployments where a specific user identity is needed.

```toml
[[parties]]
dec_party_id = "vault-network::1220..."
member_party_id = "member1::1220..."
user_id = "alice"

[parties.keycloak]
url = "https://keycloak.example.com"
realm = "canton"
client_id = "dpm-ui"
username = "alice"
password = "alice-password"
```

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

Via the UI (Network Config page) or via API:

```bash
curl -X POST http://localhost:8080/network-config \
  -H "Content-Type: application/json" \
  -d '{
    "peers": [
      {
        "participant_id": "PAR::node1::1220abc...",
        "name": "Node 1",
        "address": "10.0.0.1",
        "port": 9000,
        "public_key": "03ab12cd..."
      },
      {
        "participant_id": "PAR::node2::1220def...",
        "name": "Node 2",
        "address": "10.0.0.2",
        "port": 9000,
        "public_key": "02ef34ab..."
      }
    ]
  }'
```

### 4. Verify Connectivity

```bash
curl http://localhost:8080/participants-status
```

Response:
```json
{
  "statuses": [
    { "id": "PAR::node1::1220abc...", "status": "CurrentNode" },
    { "id": "PAR::node2::1220def...", "status": "Connected" },
    { "id": "PAR::node3::1220ghi...", "status": "Unreachable" }
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

- All participants running and configured with each other in `peers.csv`
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
      "PAR::node2::1220def...",
      "PAR::node3::1220ghi..."
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
        { "participant_uid": "PAR::node1::1220...", "permission": "submission" },
        { "participant_uid": "PAR::node2::1220...", "permission": "submission" },
        { "participant_uid": "PAR::node3::1220...", "permission": "submission" }
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
base64 -i bitsafe-vault-governance.dar -o governance.b64
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

```bash
curl -X POST http://localhost:8080/contracts \
  -H "Content-Type: application/json" \
  -d '{
    "decentralized_party_id": "my-vault-network::1220abc...",
    "participant_ids": [
      "PAR::node1::1220...",
      "PAR::node2::1220...",
      "PAR::node3::1220..."
    ],
    "participant_parties": [
      "member1::1220...",
      "member2::1220...",
      "member3::1220..."
    ],
    "operator_party": "operator::1220...",
    "dar_files": [
      {
        "filename": "governance.dar",
        "data": "<base64-encoded-dar>"
      }
    ],
    "contracts": [
      {
        "id": "vault-governance-rules",
        "name": "VaultGovernanceRules",
        "package_id": "#bitsafe-vault-governance-v0-rc2",
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
- Governance controls vault operations (deploy, pause, limits) while topology controls membership (add/remove participants)

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
| GET | `/network-config` | Get peer list | -- | `{ "peers": [...] }` |
| POST | `/network-config` | Update peer list | `{ "peers": [...] }` | `{ "peers": [...] }` |

### Keys

| Method | Endpoint | Description | Response |
|--------|----------|-------------|----------|
| GET | `/keys/status` | Get Noise keypair status | `{ "has_keys": bool, "public_key": "hex..." }` |

### Parties

| Method | Endpoint | Description | Query Params | Response |
|--------|----------|-------------|--------------|----------|
| GET | `/decentralized-parties` | List decentralized parties | `prefix` (optional) | `{ "parties": [...] }` |
| GET | `/participants-status` | Peer connectivity status | -- | `{ "statuses": [...] }` |

### Workflows

| Method | Endpoint | Description | Request Body |
|--------|----------|-------------|--------------|
| POST | `/onboarding` | Start onboarding | `{ "party_id_prefix": "...", "peer_ids": [...] }` |
| GET | `/onboarding/status` | Get onboarding progress | -- |
| POST | `/kick` | Start kick workflow | `{ "decentralized_party_id": "...", "participant_id": "...", "namespace_fingerprint": "...", "new_threshold": N }` |
| GET | `/kick/status` | Get kick progress | -- |
| POST | `/contracts` | Start contracts workflow | `{ "decentralized_party_id": "...", "participant_ids": [...], ... }` |
| GET | `/contracts/status` | Get contracts progress | -- |

### Invitations

| Method | Endpoint | Description | Request Body |
|--------|----------|-------------|--------------|
| GET | `/invitations` | List pending invitations | -- |
| POST | `/invitations/accept` | Accept an invitation | `{ "id": "..." }` |
| POST | `/invitations/decline` | Decline an invitation | `{ "id": "..." }` |

### Authentication

| Method | Endpoint | Description | Response |
|--------|----------|-------------|----------|
| GET | `/auth/status` | Get auth status for all parties | `{ "parties": [...] }` |
| POST | `/auth/test` | Test authentication | `{ "results": [...] }` |

### Governance

| Method | Endpoint | Description | Query/Body |
|--------|----------|-------------|------------|
| GET | `/governance/confirmations` | List confirmations grouped by action | `?party_id=...` |
| GET | `/governance/state` | Get governance contract state | `?party_id=...` |
| POST | `/governance/confirm` | Submit a governance confirmation | `{ "party_id": "...", "rules_contract_id": "...", "action": { "type": "...", ... } }` |
| POST | `/governance/execute` | Execute a confirmed action | `{ "party_id": "...", "rules_contract_id": "...", "action": { ... }, "confirmation_cids": [...] }` |
| POST | `/governance/expire` | Expire stale confirmation | `{ "party_id": "...", "rules_contract_id": "...", "confirmation_cid": "..." }` |

### Contracts and Services

| Method | Endpoint | Description | Query Params |
|--------|----------|-------------|--------------|
| GET | `/vaults` | List deployed Vault contracts | `?party_id=...` |
| GET | `/services/provider` | List ProviderService contracts | `?party_id=...` |
| GET | `/services/user` | List UserService contracts | `?party_id=...` |

### Response Formats

**Workflow status response:**
```json
{
  "status": "idle | inprogress | completed | failed",
  "error": "Error message if failed, null otherwise"
}
```

**Governance confirmations response:**
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
