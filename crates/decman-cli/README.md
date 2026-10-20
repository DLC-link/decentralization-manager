# decman-cli

`decman-cli` is a client for the decman HTTP API. It has two modes:

- Without a command, it opens a terminal UI.
- With a command, it runs that one action and prints the result as JSON.
  Scripts and agents use this mode.

```bash
cargo run -p decman-cli                      # terminal UI
cargo run -p decman-cli -- parties list      # one command
```

## Log in

### Profiles

A profile is one login to one decman node. Profiles live in `config.toml` in
the current directory. Use `--config <path>` or `DECMAN_CONFIG` to read another
file. Copy [config.toml.example](config.toml.example) to start. Do not commit
the file.

```toml
[[profiles]]
name = "devnet"
api_url = "https://decman.devnet.example"
username = "alice"
password_command = "pass show decman/devnet"
```

A profile holds `password` or `password_command`, not both.
`password_command` is a shell command that prints the password on its first
line, for example a password manager CLI. The command runs only when the CLI
must log in with the password.

A command picks its login in this order:

1. The profile that `-p <name>` or `DECMAN_PROFILE` names.
2. `--api-url`, `--username`, and `--password` or `--password-command`, or
   their environment variables (`DECMAN_API_URL`, `DECMAN_USERNAME`,
   `DECMAN_PASSWORD`, `DECMAN_PASSWORD_COMMAND`).
3. The only profile in the config file. With more than one profile, you must
   pick one.

The CLI loads `.env` from the current directory first, so a `.env` with
`DECMAN_API_URL` comes before a single profile.

`decman-cli profiles` lists the profiles. It never shows a password.

### Tokens

The CLI logs in with the OAuth2 password grant, as the web frontend does. It
finds the token endpoint and client id through the node's `/auth-config`. The
Keycloak client must allow "Direct Access Grants".

The command mode keeps the tokens in `$XDG_CACHE_HOME/decman-cli/`, or in
`~/.cache/decman-cli/` when `XDG_CACHE_HOME` is not set. There is one file per
API URL and username, and only the current user can read it.

- A valid access token is used again.
- An expired access token is refreshed with the refresh token.
- The password login runs only when the refresh fails.

`decman-cli login` logs in and prints the token expiry. `decman-cli logout`
deletes the cached tokens. `--no-token-cache` turns the cache off for one call.

### Protect a node

Set `require_yes = true` on a profile to protect a node from changes by
accident. Every command that sends a request other than `GET` then fails
unless you add `--yes`. The protection also applies when a command reaches
that node through `--api-url` or `DECMAN_API_URL`.

## Output

- **stdout** holds the result as JSON. Add `--compact` for one line.
- **stderr** holds progress lines and errors.
- **Exit code** is `0` on success and `1` on any failure. A workflow run that
  ends `failed` or `cancelled` also exits `1`, and its run still goes to
  stdout. Usage errors exit `2`.

Most commands print the server response as it is. The API reference is the
Swagger UI, which a node in `--insecure` mode serves at `/swagger-ui/`.

## Commands

Run `decman-cli <command> --help` for every option.

| Command | What it does |
|---|---|
| `node config \| health \| keys \| operator \| network-info` | Read this node's configuration and health |
| `peers status \| config \| set-config` | Read and set the peer list |
| `parties list \| external` | List decentralized and external parties |
| `parties create \| add-member \| remove-member \| set-threshold` | Start the onboarding, add-party, kick, and change-threshold workflows |
| `packages vetted \| expected \| compare \| config` | Read packages and compare them with peers |
| `dars upload \| distribute \| distribute-package` | Upload DARs to this node, or distribute them to peers |
| `invitations list \| handled \| accept \| decline` | Handle invitations from other nodes |
| `workflows list \| get \| wait \| cancel \| dismiss \| retry` | Handle workflow runs |
| `contracts deploy \| deploy-governance` | Deploy contracts, or the governance rules (Governance Core) from the party's known members |
| `party-config get \| set \| discover-member-party` | Read and set a party's ledger credentials |
| `auth status \| test \| grant-rights` | Check and grant the Canton rights of the party credentials |
| `governance state \| pending \| proposals \| known-members \| audit \| chain-audit` | Read the governance state |
| `governance propose \| confirm \| execute \| revoke \| expire \| cancel-proposal` | Change the governance state |
| `ledger list <resource> \| query` | Read a party's contracts |
| `api <METHOD> <PATH>` | Send any request, for a route without a command |

### Parties

`--party` takes a full party id, or the party name (the part before `::`) when
only one party on the node has that name.

### Wait for a workflow

A command that starts a workflow returns when the server accepts it. Add
`--wait` to poll until the run ends. Progress goes to stderr. `--timeout
<seconds>` stops the wait. `workflows wait <name>` waits for a run that is
already in progress.

`invitations accept <id> --wait` waits for this node's side of the workflow.

### Governance

`confirm`, `execute`, and `revoke` need the action they act on:

- `--proposal <cid>` names an on-chain proposal.
- `--confirmation <cid>` names the action that holds that confirmation.
- `--action <json>` names a self-governance action by its JSON. With
  `confirm`, the action can be new. This is how a member starts it.

The CLI reads `governance pending` and builds the same request body as the web
frontend. `execute` fails until the action has enough confirmations.

### Governance Core

`contracts deploy-governance` builds the same body as the terminal UI. The
members are the party's known members that have a member party. You give the
threshold and the confirmation timeout. Add `--dry-run` to print the body and
not send it.

### JSON input

Options such as `--body`, `--action`, and `--disclosed` take inline JSON,
`@file` for a file, or `-` for stdin. Give bodies with secrets as a file or on
stdin, so that the secrets are not in the shell history. `auth grant-rights`
reads the admin client secret from `DECMAN_ADMIN_CLIENT_SECRET` only.

## Examples

```bash
# Distribute a DAR to two peers and wait for the run to end.
decman-cli -p devnet dars distribute my-app-1.0.0.dar \
  --peer PAR::1220aa... --peer PAR::1220bb... --wait

# On each peer node: accept the invitation and wait.
decman-cli -p peer-1 invitations list
decman-cli -p peer-1 invitations accept <id> --wait

# Confirm and execute an on-chain proposal.
decman-cli -p devnet governance pending --party my-network
decman-cli -p devnet governance confirm --party my-network --proposal <cid>
decman-cli -p devnet governance execute --party my-network --proposal <cid>

# Any route, for example the governance proposals page.
decman-cli -p devnet api GET /governance/proposals -q party_id=my-network::1220...
```

Some tasks need a call on each member's node. Examples are accepting an
invitation and confirming a governance action. The CLI acts only on the nodes
you have a profile for.
