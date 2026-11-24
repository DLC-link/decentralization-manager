# decman-dapp

`decman-dapp` is the browser-only DecMan dApp. Owners use it to create a party
that several owners control, and to approve the requests that connected dApps
send to that party.

This version is a mock. The wallets, the ledger, the hosting approval and the
connected dApps are simulated in the browser. The checks are real:

- The party id, which is the decentralized namespace of the owners' keys.
- The digest that each owner signs.
- The Ed25519 check of every signature.

These checks are Rust code in this crate, compiled to WebAssembly. The server
uses the same namespace code from the `common` crate.

## Run it

You need Node 20.19 or later, the `wasm32-unknown-unknown` Rust target, and
[`wasm-pack`](https://rustwasm.github.io/wasm-pack/).

```sh
rustup target add wasm32-unknown-unknown
cd crates/decman-dapp/frontend
npm install
npm run dev
```

`npm run dev` and `npm run build` build the WebAssembly module first, into
`frontend/src/wasm`.

## Try the flow

1. On **Wallets**, add two or more wallets, each with its own participant.
2. On **New party**, pick the owners and the thresholds. The page shows the
   party id, or the reason the proposal is invalid.
3. On **Proposals**, sign as each owner. Use **Acting as** to switch owner.
   Every owner must sign before the party exists.
4. Open the party. Simulate an incoming transfer, a dApp connection and a
   transfer request. Sign the request with enough owners, then submit it.

**Reset mock data** deletes everything that the mock keeps in the browser.

## Layout

- `src/` holds the Rust checks. `cargo test -p decman-dapp` runs their tests.
- `frontend/src/checks.ts` wraps the WebAssembly exports with TypeScript types.
- `frontend/src/mock/` holds the simulated wallets, ledger and dApps. The real
  dApp replaces this folder with the JSON Ledger API and WalletConnect.
- `frontend/src/pages/` holds one file per page.
