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
