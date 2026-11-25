# decman-dapp

`decman-dapp` is the browser-only DecMan dApp. Owners use it to create a party
that several owners control, and to approve the requests that connected dApps
send to that party.

## What is real and what is simulated

Party creation is real. The dApp creates the party on Canton participants
through the JSON Ledger API:

- The party id is the decentralized namespace of the owners' keys.
- The dApp builds the topology transactions in Rust: a root certificate per
  owner, the decentralized namespace, and the party-to-participant mapping.
- The mapping names the owners' keys as the party's signing keys. One key per
  owner does both jobs.
- Each owner signs one hash that covers every transaction. The browser checks
  each Ed25519 signature before it counts it.
- Every other host approves the hosting first. Then the submitting owner's
  participant receives every transaction and the signatures.
- The dApp waits until every host reports the party as local.

The rest is simulated in the browser:

- The browser keeps every owner key. A real owner keeps it in a wallet.
- All owners sign in one browser. Use **Acting as** to switch owner.
- On localnet the dApp sends each covalidator's hosting approval. This stands
  in for the covalidator's DecMan approval.
- Holdings, transfers, history and dApp connections are simulated.

The Rust code is in this crate, compiled to WebAssembly. It shares the
namespace, topology format and hash code with the server through the `common`
and `decman-lib` crates.

## Run it against a localnet

You need Node 20.19 or later, the `wasm32-unknown-unknown` Rust target,
[`wasm-pack`](https://rustwasm.github.io/wasm-pack/) and Docker.

Start a Splice localnet. The integration suite's helpers download and start
the same localnet that CI uses:

```sh
bash -c 'source integration-tests/env.sh && download_localnet && start_localnet'
```

Run the commands in this README from the repository root. Stop the localnet
with `bash -c 'source integration-tests/env.sh && stop_localnet'`.

Then start the dApp:

```sh
rustup target add wasm32-unknown-unknown
cd crates/decman-dapp/frontend
npm install
npm run dev
```

`npm run dev` and `npm run build` build the WebAssembly module first, into
`frontend/src/wasm`.

The Vite dev server proxies the localnet participants' JSON Ledger APIs, so
the browser needs no CORS:

| Path in the dApp              | Participant  | JSON Ledger API          |
| ----------------------------- | ------------ | ------------------------ |
| `/participants/app-provider`  | App provider | `http://localhost:3975`  |
| `/participants/app-user`      | App user     | `http://localhost:2975`  |
| `/participants/sv`            | SV           | `http://localhost:4975`  |

The forms fill in the localnet token. It is the public `ledger-api-user` token
from `integration-tests/env.sh`, and it works only on a localnet.

## Try it without a localnet

Turn on **Simulated participants** to use the dApp without a localnet. The
browser then answers every JSON Ledger API call itself. The Rust code still
builds the real topology transactions and checks every signature. The
participant ids, the synchronizer and the participants' answers are made up,
and the banner says so. Turn the switch off to talk to real participants
again.

## Try the flow

1. On **Wallets**, add two wallets on different participants, for example the
   app provider and the app user. The dApp reads each participant's id.
2. Optionally, add the SV as a covalidator.
3. On **New party**, pick the owners, the covalidators and the thresholds. The
   page shows the party id, or the reason the proposal is invalid.
4. Propose as one of the owners. The dApp asks that owner's participant for its
   synchronizer and topology format, and builds the transactions.
5. On **Proposals**, sign as each owner. Every owner must sign.
6. Create the party. The card shows each host's result and any error text from
   its participant.
7. Open the party. Simulate an incoming transfer, a dApp connection and a
   transfer request. Sign the request with enough owners, then submit it.

**Reset browser data** deletes everything that the dApp keeps in the browser.
It does not touch the ledger.

## Layout

- `src/` holds the Rust code. `cargo test -p decman-dapp` runs its tests.
- `frontend/src/checks.ts` wraps the WebAssembly exports with TypeScript types.
- `frontend/src/ledger.ts` calls the JSON Ledger API.
- `frontend/src/onboarding.ts` creates the party on the participants.
- `frontend/src/mock/` holds the browser keys, the stored state and the
  simulated party wallet.
- `frontend/src/pages/` holds one file per page.
