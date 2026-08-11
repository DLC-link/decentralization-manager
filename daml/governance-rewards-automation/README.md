# governance-rewards-automation-v1

A decman governance plugin (`GovernableAction` implementations) that lets a
decentralized party manage collection of its CIP-104 rewards. Each action is
proposed, threshold-confirmed, and executed through the standard governance
`propose -> confirm -> execute` flow, running with the governance party's
authority.

Under CIP-104, a featured-app party accrues `Splice.Amulet:RewardCouponV2`
coupons that must be minted into Canton Coin before they expire (~36h TTL). A
decentralized (threshold-governed) party has no wallet automation of its own, so
it delegates minting to a validator node. This package provides the two
on-ledger actions the decparty needs to stand that up:

| Action | Effect |
|---|---|
| `SetupMintingDelegation` | Create a `MintingDelegationProposal` naming a validator as the delegate that mints the decparty's coupons. |
| `AcceptExternalPartySetup` | Accept a validator-created `ExternalPartySetupProposal`, creating the decparty's `ValidatorRight` + `TransferPreapproval` on that validator — the prerequisite that makes the validator actually run reward collection for the party. |
| `SetupCouponReassignmentDelegation` | Create (or atomically replace) a `CouponReassignmentDelegation` that carries the decparty's authority and a fixed beneficiary split; enables per-round, vote-free coupon reassignment. |
| `RevokeCouponReassignmentDelegation` | Archive the decparty's `CouponReassignmentDelegation`, disabling reassignment. |

`SetupMintingDelegation` + `AcceptExternalPartySetup` close the *minting-delegation*
loop (Mode B): a validator mints the decparty's coupons. `SetupCouponReassignmentDelegation`
/ `RevokeCouponReassignmentDelegation` enable the *coupon-reassignment* loop (Mode A),
documented below.

## Prerequisites

- A `GovernanceRules` contract deployed (from `#governance-core-<version>`).
- This `governance-rewards-automation-v1` DAR uploaded and vetted on all participants. decman resolves the
  package by name (`#governance-rewards-automation-v1`), so Canton package-preference selects the highest
  vetted version.

  **Not upgrade-compatible with `governance-rewards-v1`.** This is a new package
  lineage, not a version bump of it: `CouponReassignmentDelegation` gained a
  required `dso` field, which SCU does not permit as an in-place upgrade. The
  rename avoids the clash. `governance-rewards-v1` had reached devnet only, never
  production, so no upgrade path was owed.
- The `splice-wallet` DAR (containing `Splice.Wallet.MintingDelegation`) and
  `splice-amulet` DAR (containing `Splice.AmuletRules:ExternalPartySetupProposal`,
  `Splice.Amulet:ValidatorRight`) uploaded on all participants.

## SetupMintingDelegation

Establishes the minting delegation from the decparty to a validator operator.

### How it works

1. **Governance votes `SetupMintingDelegation`.** A member proposes the action
   with the validator operator's party as `delegate`; after threshold
   confirmations, execution creates a `MintingDelegationProposal` signed by the
   decentralized party. The delegation `beneficiary` is always the decentralized
   party (the governance party) — it is the party whose reward coupons get
   minted, and the only value the governance party's authority can create.
2. **The delegate accepts out-of-band.** The validator operator accepts the
   `MintingDelegationProposal` via its wallet API
   (`acceptMintingDelegationProposal`). This is NOT part of this plugin —
   acceptance is a manual, out-of-band step.
3. **The validator's automation mints.** Once the `MintingDelegation` is active
   (and the `ValidatorRight` from `AcceptExternalPartySetup` below exists), the
   validator's built-in `MintingDelegationCollectRewardsTrigger` periodically
   (~every 5 minutes) mints the decparty's `RewardCouponV2` coupons into Canton
   Coin, ahead of their expiry.

### Proposing

```bash
curl -X POST http://<decman-node>:8080/governance/propose \
  -H "Content-Type: application/json" \
  -d '{
    "party_id": "<decparty>::1220...",
    "rules_contract_id": "<governance-rules-cid>",
    "proposal": {
      "type": "setup_minting_delegation",
      "delegate": "<validator-operator>::1220...",
      "dso": "<dso-party>::1220...",
      "expires_at_micros": 1800000000000000,
      "amulet_merge_limit": 10,
      "description": "Collect CIP-104 rewards via validator X"
    }
  }'
```

- `delegate`: the validator node operator party that will mint on the decparty's behalf.
- `dso`: the Splice DSO party; the delegation's mint transfers verify the `AmuletRules` contract belongs to it.
- `expires_at_micros`: delegation expiry as microseconds since epoch.
- `amulet_merge_limit`: number of amulet contracts to keep after auto-merging (must be positive).

### Caveats

- **Acceptance is manual.** There is no auto-accept; the delegation does nothing until the delegate accepts the proposal via the wallet API.
- **No auto-renewal.** `expiresAt` on a `MintingDelegation` is immutable. A new `SetupMintingDelegation` vote is required before expiry to keep collecting rewards (the accept choice can atomically archive the old delegation and create the new one).

## AcceptExternalPartySetup

A `MintingDelegation` alone is not enough: the validator only spins up the
external-party wallet (and its `MintingDelegationCollectRewardsTrigger`) once the
party has a `ValidatorRight` + `TransferPreapproval` on that validator.
Establishing those is a two-step onboarding.

### How it works

1. **Validator operator creates the proposal (out of scope for this plugin).**
   The operator of the validator node calls the validator's internal admin API:

   ```bash
   curl -X POST http://<validator>:5003/api/validator/v0/admin/external-party/setup-proposal \
     -H "Content-Type: application/json" \
     -d '{ "user_party_id": "<decparty>::1220..." }'
   ```

   This creates an on-ledger `Splice.AmuletRules:ExternalPartySetupProposal`
   (signed by the validator + DSO, observed by the decparty) and returns its
   contract id. Manual operator action — not part of `governance-rewards-automation-v1`.

2. **Governance votes `AcceptExternalPartySetup`.** A member proposes the action
   with the proposal's contract id; after threshold confirmations, execution
   exercises `ExternalPartySetupProposal_Accept` with the decparty's (the
   governance party's) authority — it is the proposal's `user`, i.e. the choice
   controller. Acceptance creates the party's `ValidatorRight` and
   `TransferPreapproval` (the validator/DSO authority for those rides in on the
   consumed proposal). The validator's `ValidatorRightTrigger` then provisions
   the external-party wallet, and its `MintingDelegationCollectRewardsTrigger`
   begins collecting the party's coupons via the `MintingDelegation` above.

   ```bash
   curl -X POST http://<decman-node>:8080/governance/propose \
     -H "Content-Type: application/json" \
     -d '{
       "party_id": "<decparty>::1220...",
       "rules_contract_id": "<governance-rules-cid>",
       "proposal": {
         "type": "accept_external_party_setup",
         "proposal_cid": "<external-party-setup-proposal-cid>"
       }
     }'
   ```

   - `proposal_cid`: the `ExternalPartySetupProposal` contract id returned by step 1.

### Caveats

- **Accept promptly.** The `ExternalPartySetupProposal` carries a
  `preapprovalExpiresAt` deadline (the validator's
  `transferPreapproval.preapprovalLifetime`, default 90 days).
  `ExternalPartySetupProposal_Accept` asserts the deadline has not passed, so an
  expired proposal cannot be accepted and the operator must create a fresh one.
- **SCU cross-version.** On-ledger the proposal is created by the validator's
  splice-amulet (e.g. 0.1.22 on splice 0.6.12), while this plugin compiles
  against the vendored `splice-amulet-0.1.17`. Smart Contract Upgrade lets the
  0.1.17-typed exercise run on the newer on-ledger contract; this was verified on
  devnet (the accept executed and minting followed). Re-confirm when the
  on-ledger splice-amulet version moves substantially ahead.

## Coupon reassignment (Mode A)

An alternative to minting delegation. Instead of a validator minting the
decparty's coupons, the decparty reassigns each coupon's `beneficiary` to a
governance-approved split; the beneficiaries then self-mint. This suits a
decparty that wants rewards routed to specific parties by a fixed split, without
a per-round governance vote.

### Templates

| Template | Role |
|---|---|
| `CouponReassignmentDelegation` | Standing delegation: `signatory decparty`, `observer assigners`, with the beneficiary `split` baked in. Created once by governance. |
| `SetupCouponReassignmentDelegation` | `GovernableAction` that creates or atomically replaces the delegation (the only reassignment step that takes a vote). |
| `RevokeCouponReassignmentDelegation` | `GovernableAction` that archives the delegation. |

### How it works

1. **Governance votes `SetupCouponReassignmentDelegation` once.** After threshold
   confirmations, execution creates a `CouponReassignmentDelegation` naming the
   member parties as `assigners` and carrying the fixed `split`
   (`[{beneficiary, percentage}]`, percentages in (0,1], summing to 1.0, ≤ 20,
   no duplicates). `executeImpl` re-validates the split, so a direct ledger
   submit that bypasses the decman API cannot bake in a malformed split.
2. **Any single assigner reassigns each round — no vote.** The delegation's
   `nonconsuming` `Delegation_Assign` choice is `controller assigner` gated by
   `assert (assigner elem assigners)` (a genuine 1-of-n, not escalatable). It
   reassigns caller-supplied coupons to the contract's `split` —
   `newBeneficiaries = split`, **never** a caller argument (the security
   boundary). The per-node `reward_automation` loop drives this on a timer.
3. **Beneficiaries self-mint** the resulting coupons before expiry (out of scope
   for this plugin).

### Trust model

A single assigner (even the least-trusted member) can unilaterally reassign all
the decparty's unassigned coupons at any time — but only ever to the
governance-approved split. The blast radius is timing, not destination.
`Delegation_Revoke`/replace is `controller decparty`, i.e. governance-only.

### Proposing / revoking

```bash
# create or replace the delegation
curl -X POST http://<decman-node>:8080/governance/propose -H "Content-Type: application/json" -d '{
  "party_id": "<decparty>::1220...",
  "rules_contract_id": "<governance-rules-cid>",
  "proposal": {
    "type": "setup_coupon_reassignment_delegation",
    "assigners": ["<member1>::1220...", "<member2>::1220..."],
    "new_beneficiaries": [
      {"beneficiary": "<benef>::1220...", "percentage": "0.8"},
      {"beneficiary": "<operator>::1220...", "percentage": "0.2"}
    ],
    "prior_delegation": null
  }
}'

# revoke it
curl -X POST http://<decman-node>:8080/governance/propose -H "Content-Type: application/json" -d '{
  "party_id": "<decparty>::1220...",
  "rules_contract_id": "<governance-rules-cid>",
  "proposal": { "type": "revoke_coupon_reassignment_delegation", "delegation": "<delegation-cid>" }
}'
```

- `assigners`: member parties allowed to run per-round reassignment (1-of-n).
- `new_beneficiaries`: the fixed split; validated at propose (decman) and at execute (DAML).
- `prior_delegation`: cid of the delegation to replace (`null` for the first create).

### Caveats

- **Mutually exclusive with Mode B.** Do not run coupon reassignment and
  minting-delegation collection on the same decparty — they compete for the same
  coupons.
- **Reassignment cadence.** The `reward_automation` loop assigns every
  assignable coupon each tick, in chunks sized by output creates
  (`reward_max_creates / beneficiary_count`). Throughput does not depend on the tick
  interval, so the interval only trades assignment latency against transaction
  cost.
