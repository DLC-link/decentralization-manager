# Canton Decentralized Party Manager - User Guide

This is an operator quick-start. The application is configured entirely through
`DECPM_*` environment variables (or a `.env` file placed in the directory given
by `--dir` / `DECPM_DIR`).
There is **no TOML config file** — every setting is an env var / CLI flag.

For end-to-end workflow walkthroughs (onboarding a party, deploying contracts,
kicking a participant), see the [Use Cases](docs/USE_CASES.md).

## Quick Start with Docker

Build the image locally, then run a single instance:

```bash
# Build the image
docker build -t dec-party-manager .

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

## Configuration

All configuration is supplied via `DECPM_*` environment variables. The key ones:

| Variable | Description | Default |
|----------|-------------|---------|
| `DECPM_PORT` | Port for the HTTP / web UI server | `8080` |
| `DECPM_NOISE_PORT` | Port for the Noise P2P transport | `9000` |
| `DECPM_CANTON_ADMIN_HOST` | Canton Admin API host | `127.0.0.1` |
| `DECPM_CANTON_ADMIN_PORT` | Canton Admin API port | `5002` |
| `DECPM_CANTON_LEDGER_HOST` | Canton Ledger API host | `127.0.0.1` |
| `DECPM_CANTON_LEDGER_PORT` | Canton Ledger API port | `5001` |
| `DECPM_CANTON_SYNCHRONIZER` | Canton synchronizer name | `global` |
| `DECPM_CANTON_NETWORK` | Canton network (`devnet`, `testnet`, `mainnet`) | `devnet` |

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
