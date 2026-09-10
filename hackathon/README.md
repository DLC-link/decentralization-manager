# DecMan on LocalNet

A one-command Canton Network sandbox for hackathon teams. It gives you three
Canton participants, one local synchronizer, and three DecMan nodes that already
know each other. There is no login, no identity provider, and no Rust toolchain.
The DecMan nodes run the published release image as is.

```bash
./hackathon/up.sh      # start (first run downloads about 760MB, then pulls images)
./hackathon/seed.sh    # create a demo party and deploy the governance core
./hackathon/demo.sh    # run one propose, confirm and execute, then print the evidence
./hackathon/down.sh    # stop, keep the data
./hackathon/reset.sh   # delete all data and start from zero
```

After `up.sh` finishes:

| Node | UI | Canton participant |
| --- | --- | --- |
| DecMan 1 | http://localhost:8081 | app-provider (`canton:3901` / `canton:3902`) |
| DecMan 2 | http://localhost:8082 | app-user (`canton:2901` / `canton:2902`) |
| DecMan 3 | http://localhost:8083 | sv (`canton:4901` / `canton:4902`) |

The API browser is at http://localhost:8081/swagger-ui/.

For the click-through tour, read [WALKTHROUGH.md](WALKTHROUGH.md). It takes
about 30 minutes and ends with an executed governance action in the audit trail.

## Prerequisites

- Docker Desktop, or any Docker with Compose v2.1.1 or newer.
- **12GB of memory and 4 CPUs for Docker.** LocalNet reserves about 9GB for
  Canton, Splice and Postgres. Docker Desktop: Settings > Resources. `up.sh`
  warns you if the limits are too low.
- About 20GB of free disk for the bundle, the images and the ledger.
- `curl`, `jq` 1.6 or newer, `base64`, `tar`, and Bash. macOS and Linux both work.
- A clone of this repository. The scripts read the governance DARs from
  `releases/v1/`.

Apple Silicon: the Canton and Splice images are multi-arch and run native, but
the DecMan release image is `linux/amd64` only, so Docker runs the three DecMan
containers under emulation. The compose file pins `platform: linux/amd64` so the
pull does not fail. Expect a slower start and higher CPU use on those three
containers.

If `DOCKER_DEFAULT_PLATFORM=linux/amd64` is set in your shell, everything in the
stack runs emulated, Canton included. Unset it for a native LocalNet.

## What runs

`up.sh` does five things:

1. Downloads and caches the Splice LocalNet bundle in `.localnet/` at the repo
   root — the same cache the integration-test harness uses, so you download it
   once.
2. Starts the bundle's `canton`, `splice` and `postgres` services with the `sv`,
   `app-provider` and `app-user` profiles, and waits for the health checks.
3. Starts three DecMan containers from the pinned release image on the bundle's
   Docker network, so they reach Canton as `canton:<port>`.
4. Writes the peer mesh: each node learns the other two Noise public keys, then
   the nodes restart to load them.
5. Waits until all three nodes report each other as connected.

Re-running `up.sh` is safe. It keeps the ledger and the DecMan databases, and it
skips the peer setup when the mesh is already up.

Versions live in one place, [versions.env](versions.env): the LocalNet bundle
version and the DecMan image tag. The bring-up itself lives in
[localnet.sh](localnet.sh), which `integration-tests/env.sh` sources as well, so
CI and this bundle boot LocalNet exactly the same way. The harness wipes the
ledger on start because tests must own it; this bundle keeps it.

## What LocalNet is not

LocalNet is a sandbox. Four things differ from a real deployment:

- **No DSO.** The demo party's own member party on node 1 stands in for the DSO
  and for the app operator. On DevNet and MainNet both are separate, real parties.
- **No CBTC, no real Canton Coin, no Utility credential.** The token-custody and
  utility-onboarding packages are distributed, but there is no registry operator
  to onboard against.
- **Authentication is off.** The nodes run with `DECPM_INSECURE=true`, so the API
  accepts any token and DecMan presents an unsafe HMAC token to Canton. A real
  node validates JWTs against an identity provider. The guard in DecMan refuses
  this flag on any network other than `devnet`.
- **One Canton container hosts all three participants.** In production each
  participant is its own node, and DecMan reaches it over the Admin and Ledger
  APIs across the network.

For a production-shaped deployment, read [../docs/DEPLOYMENT_GUIDE.md](../docs/DEPLOYMENT_GUIDE.md).

## Troubleshooting

**A port is already taken.** `up.sh` stops when 8081, 8082 or 8083 is busy. The
LocalNet bundle also publishes ports in the 2900-4999 range; if Compose reports
`port is already allocated`, stop the process that holds it. A LocalNet from the
integration-test harness (`integration-tests/run.sh`) uses the same ports, so
stop that one first.

**Canton or Splice never becomes healthy.** This is nearly always the memory
limit. Raise Docker to 12GB, then `./hackathon/reset.sh` and `./hackathon/up.sh`.
Watch it with:

```bash
docker logs -f canton
docker logs -f splice
```

**The DecMan image will not pull.** `pull access denied` or `authorization token
has expired` on `public.ecr.aws` means your Docker config holds a stale ECR
login. The image is public and needs none, so drop the login:

```bash
docker logout public.ecr.aws
```

**The download fails.** Delete `.localnet/` at the repo root and run `up.sh`
again. The
bundle comes from the `digital-asset/decentralized-canton-sync` GitHub release,
which needs no credentials.

**The nodes do not see each other.** Check the mesh from the API:

```bash
curl -s localhost:8081/participants-status | jq
```

`Connected` and `CurrentNode` are good. `Unreachable` means the Noise listener
is not answering; `HandshakeFailed` means the peer's public key is wrong. Re-run
`up.sh` to rewrite the mesh, and read the logs:

```bash
docker compose -p decman-hackathon logs -f
```

**A workflow hangs.** Every workflow needs the two peers to accept an
invitation. Look at `GET /invitations` on nodes 2 and 3, or open the UI. To
abandon a run, use the cancel endpoint for its kind, for example
`POST /onboarding/cancel`.

**Start over.** `./hackathon/reset.sh` deletes the ledger, the party and the
three DecMan databases. It keeps the downloaded bundle.
