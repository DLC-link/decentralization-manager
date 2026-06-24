# DPM UI e2e tests

Playwright UI tests mirroring the non-chaos phases of the Rust integration test, run as Keycloak user `cvault-finoa-lp-1`.

## Prerequisites
- Node `^20.19.0 || >=22.12.0`, then `npm install && npx playwright install chromium`.
- Devnet tunnels up + `~/.config/dec-party-manager/devnet.env` (same as `integration-tests/run.sh --target devnet`).
- `E2E_KC_PASSWORD` exported (1Password locally). Never commit it.

## Run
```bash
# Source the devnet env first — the deploy_gov_core phase reads member/admin
# creds (P{1,2,3}_MEMBER_*, P{1,2,3}_PARTICIPANT_ADMIN_*) from the environment.
set -a; source ../integration-tests/devnet.env.sh; set +a

cd e2e
npm test                 # globalSetup brings up the stack on :8081-8083
# Faster iteration against an already-running stack:
./../integration-tests/bring-up.sh
E2E_ASSUME_STACK_UP=1 npm test
./../integration-tests/bring-up.sh --teardown
```

Opt-in only: not part of default CI (needs devnet creds + tunnels + a built `release-ci` binary).

## Test report

Every run writes a Playwright HTML report to `playwright-report/` (gitignored). Inspect it:

```bash
npm run report          # serves the last report at http://localhost:9323
# equivalently: npx playwright show-report
```

The report has a per-case pass/fail summary, a step timeline for each case, and the full-page screenshots each phase attaches at its end. On failure, Playwright also drops a screenshot and a trace under `test-results/<case>/`; open a trace with:

```bash
npx playwright show-trace test-results/<case>/trace.zip
```

Both `playwright-report/` and `test-results/` are regenerated on each run and are gitignored — nothing report-related is committed.

## Known coverage gaps (vs. the Rust IT)
- **`generic_vote` confirm/execute go through the API, not the UI.** All three nodes log in as the same Keycloak user (`cvault-finoa-lp-1`), so the UI treats the proposer's confirmation as "yours" on every node and never offers the peer `Confirm`/`Execute` dialogs. The suite proposes via the UI and reaches threshold via the per-node `/governance/*` endpoints (as the Rust IT does). Covering the `Confirm`/`Execute` UI dialogs would require distinct, per-node frontend users; the Rust IT covers that path.
- **Phases kept in the Rust IT, not ported here:** `utility_onboarding` (mint/burn needs manually-supplied disclosed-contract blobs the UI can't auto-resolve); `token_custody` (a `propose_confirm_execute("SetupCcPreapproval", …)` like `generic_vote`, but its propose payload needs `provider` + `expected_dso`, the latter requiring DSO-party discovery on devnet — deferred for that extra setup, not a disclosed-contract limit); and `owner_key_resilience` (asserts internal invariants not surfaced in the UI).
