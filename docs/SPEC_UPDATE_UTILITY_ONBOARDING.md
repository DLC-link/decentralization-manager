# Spec Update: Token Issuance → Utility Onboarding Plugin

**Status:** pending team review. Based on scolear's PR #48 review comments.

**Supersedes / amends:** [TOKEN_ISSUANCE_PLUGIN.md](TOKEN_ISSUANCE_PLUGIN.md)

**Reference pattern:** [CBTC/Governance.daml](https://github.com/DLC-link/canton/blob/d83404be/daml/governance/daml/CBTC/Governance.daml) — used as a model, **not** as a dependency. The goal is for this plugin to become the target architecture that the current CBTC implementation eventually migrates to.

---

## 1. Scope change: "utility onboarding" not just "token issuance"

The plugin is reframed from a narrow token-issuance plugin to a broader **utility onboarding plugin**. Token issuance (mint + burn) remains the end result, but the plugin now also covers the individual onboarding steps that precede it.

**Package rename:** `governance-token-issuance` → `governance-utility-onboarding`.

---

## 2. Drop `IssuanceConfig` state contract

The original spec introduced `IssuanceConfig` as a per-deployment state contract holding `allocationFactoryCid`, `instrumentConfigurationCid`, and UX metadata. This is removed.

**New approach:** all contract IDs (`allocationFactoryCid`, `instrumentConfigurationCid`, `registrarServiceCid`, etc.) are passed **directly as fields** on each proposal template. No custom intermediate template.

**Rationale (scolear):** "I would suggest we don't introduce this custom Template at all, and lets instead rely on the Utility's models directly." Passing CIDs directly also makes transactions cheaper (no extra fetch).

**Consequences:**
- `IssuanceConfig` template — **removed**
- `IssuanceConfig_RotateFactory` choice — **removed**
- `RotateFactoryProposal` — **removed** (if the factory CID changes, the committee simply uses the new CID on subsequent proposals)
- `MintProposal` / `BurnProposal` — now take `allocationFactoryCid : ContractId AllocationFactory` and `instrumentConfigurationCid : ContractId InstrumentConfiguration` as direct fields (previously referenced via `issuanceConfigCid`)
- Token UX metadata (`displayName`, `symbol`, `decimals`) — no longer stored on-chain. This is consistent with how CBTC handles it: the instrument is identified on-chain by `InstrumentId` (via `InstrumentConfiguration`), and any display-level UX (name, symbol, decimals) is handled off-chain by the frontend/backend

---

## 3. Composite + granular action pattern

The original spec had a single `SetupIssuanceProposal` that ran the entire onboarding chain in one vote. This is **kept** (renamed to `SetupUtility`), but the plugin now also exposes **individual sub-action templates** for each step.

This mirrors the CBTC Governance pattern where `CBTCGovernanceRules_SetupUtility` is the composite action, while choices like `CBTCGovernanceRules_CreateProviderServiceRequest` etc. are available for granular control.

### Composite action

**`SetupUtility`** — one committee vote runs the full onboarding chain:
1. `ProviderService_CreateProviderConfiguration`
2. Create `RegistrarServiceRequest` with `createTransferRule = Some True` and `createAllocationFactory = Some True`
3. `ProviderService_AcceptRegistrarServiceRequest`
4. `RegistrarService_CreateInstrumentConfiguration`

**Key change from original:** `createAllocationFactory = Some True` on the `RegistrarServiceRequest` eliminates the separate `RegistrarService_CreateAllocationFactory` step (per scolear: "You can get rid of this if you make `createAllocationFactory` true on line 66"). Similarly, `createTransferRule = Some True` creates a `TransferRule` automatically ("We should also make a TransferRule the same way").

### Individual sub-action templates (new)

Each is its own `GovernableAction` template, taking the relevant CIDs as fields:

| Template | Wraps | Key CID field(s) |
|---|---|---|
| `CreateProviderServiceRequest` | Creates a `ProviderServiceRequest` | `operator` |
| `CreateUserServiceRequest` | Creates a `UserServiceRequest` | `operator` |
| `CreateTransferRule` | `RegistrarService_CreateTransferRule` | `registrarServiceCid` |
| `SetProviderAppRewardBeneficiaries` | `InstrumentConfiguration_SetProviderAppRewardBeneficiaries` | `instrumentConfigurationCid` |
| `SetEnableResultContracts` | `RegistrarService_Set` with `enableResultContracts` | `registrarServiceCid` |
| `CreateDelegatedBatchedMarkersProxy` | Creates a `DelegatedBatchedMarkersProxy` | `operator` |

These are **not** branches of a Daml sum type — they are independent templates following the dec-party governance pattern, modeled after the CBTC pattern.

---

## 4. No hardcoded identifiers

Per scolear's inline comments on `MintProposal.daml:71` and `MintProposal.daml:74`: "in these cases the safest is to pass it in as a param, so we can change it if needed — no hardcodes if possible."

All identifiers that were previously hardcoded in proposal templates must become configurable parameters on the proposal fields.

---

## 5. `MintProposal` / `BurnProposal` changes

Previous:
```
issuanceConfigCid : ContractId IssuanceConfig
```

New:
```
allocationFactoryCid : ContractId AllocationFactory
instrumentConfigurationCid : ContractId InstrumentConfiguration
instrumentId : InstrumentId
```

The proposals are now self-contained — they carry all the CIDs they need. `executeImpl` no longer fetches a config contract; it uses the CIDs directly.

---

## 6. `actionLabel` values (updated)

Old: `"SetupIssuance"`, `"Mint"`, `"Burn"`, `"RotateFactory"`

New:
- `"SetupUtility"` — composite setup
- `"CreateProviderServiceRequest"`
- `"CreateUserServiceRequest"`
- `"CreateTransferRule"`
- `"SetProviderAppRewardBeneficiaries"`
- `"SetEnableResultContracts"`
- `"CreateDelegatedBatchedMarkersProxy"`
- `"Mint"`
- `"Burn"`

---

## 7. Future scope (explicitly deferred)

- **Utility Credentials** — next PR, once the onboarding plugin is in place
- **`RotateFactory`** — no longer needed; if the need arises, it can be re-added as a sub-action

---

## Summary of removed / renamed / added templates

| Original spec | Updated spec | Status |
|---|---|---|
| `IssuanceConfig` | — | **Removed** |
| `IssuanceConfig_RotateFactory` | — | **Removed** |
| `SetupIssuanceProposal` | `SetupUtility` | **Renamed + modified** (uses setup flags) |
| `MintProposal` | `MintProposal` | **Modified** (CIDs as direct fields) |
| `BurnProposal` | `BurnProposal` | **Modified** (CIDs as direct fields) |
| `RotateFactoryProposal` | — | **Removed** |
| — | `CreateProviderServiceRequest` | **New** |
| — | `CreateUserServiceRequest` | **New** |
| — | `CreateTransferRule` | **New** |
| — | `SetProviderAppRewardBeneficiaries` | **New** |
| — | `SetEnableResultContracts` | **New** |
| — | `CreateDelegatedBatchedMarkersProxy` | **New** |
