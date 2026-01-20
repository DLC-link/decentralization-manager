# Canton Decentralized Party Manager

A web application for managing decentralized parties in Canton blockchain networks. Provides a user interface for onboarding new parties, deploying governance contracts, and managing participant membership.

## Features

- **Web-Based Management UI**: React frontend for managing decentralized parties
- **Multi-Party Onboarding**: Coordinated workflow for creating decentralized party namespaces
- **Contract Deployment**: Upload DAR files and deploy governance contracts with multi-party signing
- **Governance Actions**: View and manage governance confirmations with threshold-based execution
- **Participant Management**: View party membership, kick participants with threshold-based voting
- **Keycloak Authentication**: Supports M2M (client_credentials) and password flows for Ledger API access
- **Secure P2P Communication**: Noise Protocol Framework for encrypted coordinator-attestor communication
- **Real-time Status**: Live peer connectivity monitoring and workflow progress tracking
- **Canton Integration**: Native gRPC integration with Canton Admin and Ledger APIs

## Architecture

The application runs as an HTTP server with an embedded React frontend. Multiple instances coordinate via the Noise Protocol:

- **Coordinator**: Initiates workflows and orchestrates multi-party operations
- **Attestors**: Respond to coordinator commands, sign proposals, and execute local operations
- **Automatic Key Management**: Noise keypairs are generated automatically on first run

```
┌─────────────────┐     Noise Protocol      ┌─────────────────┐
│  Participant 1  │◄───────────────────────►│  Participant 2  │
│   (Coordinator) │                         │    (Attestor)   │
│   HTTP :8081    │                         │   HTTP :8082    │
│   Noise :9001   │                         │   Noise :9002   │
└────────┬────────┘                         └────────┬────────┘
         │                                           │
         │              Canton Network               │
         └───────────────────┬───────────────────────┘
                             │
                    ┌────────▼────────┐
                    │  Canton Nodes   │
                    │  (Admin/Ledger  │
                    │      APIs)      │
                    └─────────────────┘
```

## Quick Start

### Prerequisites

- Rust toolchain (for building from source)
- Access to Canton participant nodes (Admin API and Ledger API)
- Docker (optional, for containerized deployment)

### Running Locally

```bash
# Build and run
cargo run -- -d ./development/participant-1 serve --port 8081

# Or with release build
cargo build --release
./target/release/dec-party-manager -d ./development/participant-1 serve --port 8081
```

Open http://localhost:8081 in your browser.

### Running with Docker

```bash
# Build the image
docker build -t dec-party-manager .

# Run a single instance
docker run -p 8080:8080 -v ./config:/config -v ./data:/data dec-party-manager
```

### Running Multiple Participants (Development)

```bash
cd development
docker compose up
```

This starts three participant instances on ports 8081, 8082, and 8083.

## Configuration

The application expects a directory structure with `config/` and `data/` subdirectories:

```
participant-dir/
├── config/
│   ├── node.toml      # Node configuration
│   └── peers.csv      # Network peer list
└── data/
    ├── keys/          # Auto-generated Noise keypair
    └── workflow/      # Workflow data (proposals, signatures, etc.)
```

### Node Configuration (`config/node.toml`)

```toml
[node]
node_id = "participant-1"
listen_address = "0.0.0.0"
port = 9001

[canton]
admin_api_host = "localhost"
admin_api_port = 5002
ledger_api_host = "localhost"
ledger_api_port = 5001
ledger_api_user_id = "ledger-api-user"
synchronizer = "global"

[timeouts]
handshake_timeout_secs = 30
message_timeout_secs = 120
connection_retry_attempts = 3
connection_retry_delay_secs = 5
```

### Network Peers (`config/peers.csv`)

```csv
id,name,address,port,public_key,party
participant-1,Participant 1,10.0.0.1,9001,03ab12cd...,
participant-2,Participant 2,10.0.0.2,9002,02ef34ab...,
participant-3,Participant 3,10.0.0.3,9003,03cd56ef...,
```

- `id`: Unique identifier for the peer
- `name`: Display name
- `address`: Hostname or IP address for Noise connections
- `port`: Noise protocol port
- `public_key`: Hex-encoded secp256k1 public key (auto-populated from `/keys/status` endpoint)
- `party`: Canton party ID (populated after onboarding)

## Workflows

### Creating a Decentralized Party (Onboarding)

1. Configure all participant nodes with each other's connection details in `peers.csv`
2. Start all participant servers
3. On the coordinator's UI, click **Create Party** and enter a party ID prefix
4. The coordinator invites attestors and orchestrates:
   - Cryptographic key generation (namespace + DAML signing keys)
   - Topology proposal creation (DNS and P2P mappings)
   - Multi-party signing
   - Proposal submission to Canton

### Deploying Contracts

1. From a party card in the UI, click **Deploy Contracts**
2. Upload DAR files via the file picker
3. Configure contract definitions (operator party, templates, fields)
4. The coordinator orchestrates:
   - DAR distribution and upload to all participants
   - Ledger submission preparation
   - Multi-party signing of submissions
   - Execution on the Canton ledger

### Removing a Participant (Kick)

1. From a party card, click **Kick Participant**
2. Select the participant to remove
3. The coordinator orchestrates:
   - Export current namespace state
   - Create updated topology proposals (reduced threshold, removed P2P mapping)
   - Multi-party signing by remaining members
   - Proposal submission

## API Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/` | GET | Serves the React frontend |
| `/node-config` | GET | Returns node configuration |
| `/network-config` | GET | Returns network peer list |
| `/network-config` | POST | Updates network peer list |
| `/decentralized-parties` | GET | Lists decentralized parties (filtered by template) |
| `/participants-status` | GET | Returns peer connectivity status |
| `/keys/status` | GET | Returns Noise keypair status |
| `/onboarding` | POST | Starts onboarding workflow |
| `/onboarding/status` | GET | Returns onboarding progress |
| `/contracts` | POST | Starts contracts workflow |
| `/contracts/status` | GET | Returns contracts progress |
| `/kick` | POST | Starts kick workflow |
| `/kick/status` | GET | Returns kick progress |
| `/invitations` | GET | Returns pending workflow invitations |
| `/invitations/accept` | POST | Accepts a pending invitation |
| `/invitations/decline` | POST | Declines a pending invitation |
| `/auth/status` | GET | Returns authentication status for configured parties |
| `/auth/test` | POST | Tests Keycloak authentication |
| `/governance/confirmations` | GET | Returns governance confirmations grouped by action |
| `/governance/confirm` | POST | Submits a governance confirmation |
| `/governance/execute` | POST | Executes a confirmed governance action |

## Development

### Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Run tests
cargo test

# Run linter
cargo clippy --all-targets --all-features -- -D warnings

# Format code
cargo fmt
```

### Frontend Development

```bash
cd frontend
npm install
npm run dev     # Development server with hot reload
npm run build   # Production build (output to dist/)
```

The frontend is embedded into the Rust binary at build time via `build.rs`.

## Docker Image

Build and push to ECR:

```bash
# Build
docker build -t dec-party-manager .

# Tag for ECR
docker tag dec-party-manager:latest public.ecr.aws/your-repo/dec-party-manager:v1.0.0

# Push
docker push public.ecr.aws/your-repo/dec-party-manager:v1.0.0
```

## License

Proprietary - All rights reserved.
