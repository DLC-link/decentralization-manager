# Canton Decentralized Party Manager - User Guide

This is an operator quick-start. The application is configured entirely through
`DECPM_*` environment variables (or a `.env` file placed in the directory given
by `--dir` / `DECPM_DIR`).
There is **no TOML config file** — every setting is an env var / CLI flag.

For end-to-end workflow walkthroughs (onboarding a party, deploying contracts,
kicking a participant), see the [Use Cases](docs/USE_CASES.md).

## Quick Start with Docker

Build the image locally, then run a single instance. The build fetches the
`canton-lib` Rust dependency from GitHub over SSH, so forward an SSH key
registered on a GitHub account via BuildKit's `--ssh` flag (`canton-lib` is
public, so no special repository access is needed):

```bash
# Build the image (replace the key path with your own GitHub-registered key)
docker build --ssh default=$HOME/.ssh/id_ed25519 -f development/Dockerfile -t dec-party-manager .

# Run
docker run -p 8080:8080 -p 9000:9000 -v ./data:/data \
  -e DECPM_PORT=8080 \
  -e DECPM_NOISE_PORT=9000 \
  -e DECPM_CANTON_ADMIN_HOST=canton-node \
  -e DECPM_CANTON_ADMIN_PORT=5002 \
  -e DECPM_CANTON_LEDGER_HOST=canton-node \
  -e DECPM_CANTON_LEDGER_PORT=5001 \
  -e DECPM_CANTON_SYNCHRONIZER=global \
  -e DECPM_CANTON_NETWORK=devnet \
  dec-party-manager
```

Then open the web UI at `http://localhost:8080`.

The `-v ./data:/data` mount persists the SQLite database (peers, party
credentials) and the auto-generated Noise keypair across restarts.

`/` is the image's default `DECPM_DIR`, and the app writes `$DECPM_DIR/data` —
so that is the directory to mount. Point the mount somewhere else and pass
`-e DECPM_DIR=<parent>` to match, or the container writes inside its own
filesystem and the data disappears with it.

### Nonroot image

Every release is published twice — `…:<tag>` and `…:<tag>-nonroot`. Both hold
the same binary; the second runs as uid 65532 rather than root, which is the
one to deploy where a policy forbids root containers.

Its `DECPM_DIR` defaults to `/home/nonroot`, because uid 65532 cannot create
`/data`. So the mount moves with it, and the host directory has to belong to
that uid — a bind mount keeps the host's ownership, and without the `chown` the
first write fails:

```bash
mkdir -p ./data && sudo chown 65532:65532 ./data
docker run -p 8080:8080 -p 9000:9000 -v ./data:/home/nonroot/data \
  ... public.ecr.aws/dlc-link/decentralization-manager:<tag>-nonroot
```

Nothing else changes: same ports, same env vars, same entrypoint. For
Kubernetes, see the [Deployment Guide](docs/DEPLOYMENT_GUIDE.md).

## Configuration

All configuration is supplied via `DECPM_*` environment variables. The key ones:

| Variable | Description | Default |
|----------|-------------|---------|
| `DECPM_PORT` | Port for the HTTP / web UI server | `8080` |
| `DECPM_NOISE_PORT` | Port for the Noise P2P transport | `9000` |
| `DECPM_LOG_FORMAT` | Log format. Set `text` for the readable console format while working locally | `json` |
| `DECPM_CANTON_ADMIN_HOST` | Canton Admin API host | `127.0.0.1` |
| `DECPM_CANTON_ADMIN_PORT` | Canton Admin API port | `5002` |
| `DECPM_CANTON_LEDGER_HOST` | Canton Ledger API host | `127.0.0.1` |
| `DECPM_CANTON_LEDGER_PORT` | Canton Ledger API port | `5001` |
| `DECPM_CANTON_SYNCHRONIZER` | Canton synchronizer name | `global` |
| `DECPM_CANTON_NETWORK` | Canton network (`devnet`, `testnet`, `mainnet`) | `devnet` |
| `DECPM_CANTON_ADMIN_TLS` | Speak TLS to the Canton Admin API | `false` |
| `DECPM_CANTON_ADMIN_TLS_CA_CERT` | PEM of the CA that issued the Admin API certificate, when it is a private one | _(platform trust store)_ |
| `DECPM_CANTON_LEDGER_TLS` | Speak TLS to the Canton Ledger API | `false` |
| `DECPM_CANTON_LEDGER_TLS_CA_CERT` | PEM of the CA that issued the Ledger API certificate | _(platform trust store)_ |

Both Canton channels default to plaintext h2c, which is what a participant
reachable only over a trusted private network serves. If yours has TLS
enabled, set the flags above; mTLS and certificate-name overrides are covered
in the [README](README.md#tls-to-the-participant).

Instead of `-e` flags, you can place a `.env` file in the directory given by
`--dir` / `DECPM_DIR` (its root — not the `data/` subfolder). It is loaded
automatically on startup (before CLI parsing), so any `DECPM_*` key set there
takes effect:

```env
DECPM_PORT=8080
DECPM_NOISE_PORT=9000
DECPM_CANTON_ADMIN_HOST=canton-node
DECPM_CANTON_ADMIN_PORT=5002
DECPM_CANTON_LEDGER_HOST=canton-node
DECPM_CANTON_LEDGER_PORT=5001
DECPM_CANTON_SYNCHRONIZER=global
DECPM_CANTON_NETWORK=devnet
```

## Port Requirements

| Port | Purpose |
|------|---------|
| 8080 | HTTP / web UI (default, `DECPM_PORT`) |
| 9000 | Noise P2P communication between participants (default, `DECPM_NOISE_PORT`) |

For P2P to work, the Noise port must be reachable by the other participants.

## Next Steps

- **Walk through onboarding, deploying contracts, and kicking a participant** —
  [Use Cases](docs/USE_CASES.md)
