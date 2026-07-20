# CIP-104 Coupon-Reassignment Automation (Mode A) — Design

**Date:** 2026-07-14 (rev. 2026-07-20)
**Status:** Design — reconciled with delivered code (Mode A M1–M3 shipped; M4 devnet IT pending)
**Author:** Gyorgy Balazsi (with Claude)
**Design doc lives in:** `cip-104` · **Implementation target:** `decentralization-manager`

## In short

A consortium of orgs is represented on-chain by a single **decentralized party id** (`cbtc-network::1220…`). Under CIP-104 it earns rewards as `RewardCouponV2` coupon contracts; the party credited on each coupon is its **`provider`** field, which here is that decparty id.

But in that on-chain capacity the consortium has **no wallet** — it can act only through threshold governance. That is slow, because acting means the member orgs **vote by hand**: an operator at each org clicks "confirm." Coupons arrive every round and expire in ~36h, so manual voting can't keep up, and the rewards expire uncollected.

So this builds **automation, running on every member's node, that casts those votes automatically**. It routes each coupon to its governance-configured recipients — technically, it *assigns* the coupon: the unassigned coupon (whose `beneficiary` field is empty) is replaced by one coupon per recipient, each carrying its own `beneficiary` and its share of the reward `amount`.

The safety comes from the votes, not from trusting any node: each node submits its confirmation **only** for a routing that matches the on-ledger rules. So no node is trusted, and no human clicks.

## Glossary

*Actor model (used throughout — see §2):*
- **Business entity (L1)** — the org, or group of orgs, that *makes decisions*. Off-chain.
- **Software agent (L2)** — the running software that *acts* for an entity: holds keys, submits transactions. A validator node or a wallet.
- **Party id (L3)** — the on-chain name (`foo::1220…`) that *represents* an entity. Passive: acted upon, never acting.

*Canton / parties:*
- **Validator node** — a Canton participant node; generic infrastructure any org runs. "Validator" alone says nothing about whose it is or in what role.
- **Consortium member** — one of the m-of-n orgs that jointly govern a decentralized party. Each wears **two hats**: a *decision-maker* co-governing the decparty (a business entity, L1), and a *node operator* whose participant + DecMan instance is a software agent (L2) — here, the node that runs the automation. Same org, two layers.
- **Decentralized party (decparty)** — one party id collectively controlled by a consortium via an m-of-n threshold (here, `cbtc-network`). No single member can act for it alone.
- **Threshold governance (propose → confirm → execute)** — DecMan's on-ledger flow by which a consortium acts as its decparty: a member proposes an action, members confirm (each submitting a `GovernanceConfirmation` — its "vote"), and once ≥ threshold have confirmed, any member executes. Currently human-driven (hence slow).

*CIP-104 rewards:*
- **App provider (provider party)** — the party the DSO credits with app rewards. Here the decparty `cbtc-network` is its own provider.
- **`RewardCouponV2` / coupon** — a *minting right* (not CC) issued per round; must be redeemed before it expires (**TTL ≈ 36h**).
- **Beneficiary** — the party a coupon's value is assigned to; it (its agent) mints the coupon.
- **Assign (`RewardCoupon_AssignBeneficiaries`)** — a choice the provider party controls that replaces an unassigned coupon with one coupon per beneficiary (each carrying its `beneficiary` + percentage); afterwards each beneficiary can mint its share.
- **Self-mint** — a beneficiary's *agent* consuming its coupon to produce CC. Non-custodial: the CC lands with the beneficiary.
- **DSO** — the Splice super-validator collective that issues coupons / runs Amulet rules.
- **Effective split** — the resolved `{beneficiary → percentage}` set (for CBTC, an example configured value: operator 20% + `cbtc-beneficiary` 80%), read from the on-ledger `RewardSplitConfig` (§8).

*This design:*
- **Coupon-reassignment automation** ("the automation") — the per-node background process this doc specifies (a member node's L2 agent).
- **Proposer / Confirmer** — the automation's two roles: propose a batched assignment; validate + auto- confirm proposals that match the on-ledger split.
- **Auto-confirmation** — the net-new mechanism: a node confirms an action automatically *iff* it matches on-ledger policy — replacing the human click, without weakening the threshold.
- **Mode A / Mode B** — two alternative reward-distribution approaches, **not** runtime-selectable via a mode flag. A = assign & self-mint (this doc), a standing per-node automation. B = collect coupons into the decparty via Robert's `MintingDelegation`/`AcceptExternalPartySetup` path (a one-shot, shipped as PR #256). There is no shared mode selector; a decparty runs one approach or the other.

## 1. Context & goal

Under CIP-104, a consortium acting as an app provider — its **decparty id** is the coupon's `provider` — earns traffic-based app rewards as `RewardCouponV2` coupons. Each coupon is a *minting right* that must be redeemed (or have beneficiaries assigned) before it expires — default TTL **36h**.

The consortium cannot use the standard single-node wallet automation in its decparty capacity. Why is precise once you name the actors (§2): a wallet is a *single software agent* bound to a *single* entity's authority, but a decparty's authority is a **threshold over many agents**. So the members can act as the decparty only through slow threshold governance, with no standing, per-round way to act on the coupons. (You could call the reward engine a purpose-built, threshold-aware narrow subset of wallet functionality.)

Verified on devnet (PQS `pqs_cbtc`, snapshot round 51961 / 2026-07-09): there are 118 `RewardCouponV2` coupons (~43,303 CC) with `provider = cbtc-network` (the CBTC decparty), all with `beneficiary = null` — **0 collected, all expired unclaimed**, and **0 coupons ever assigned to the configured beneficiary**. This is the gap this work addresses.

**Goal:** the first production increment of the DecMan reward engine — **Mode A (assign & self-mint)**: per-node automation ("the automation") that discovers the decparty's unassigned coupons and assigns the governance-configured beneficiary split via `RewardCoupon_AssignBeneficiaries`, so each beneficiary can mint its share. **In essence it automates the committee's per-round voting** — the m-of-n `GovernanceConfirmation` submissions that, if left to human operators clicking "confirm" on every coupon batch, could never keep pace with per-round coupons expiring in 36h. Built to keep (code quality, tests), exercised on devnet.

**Scope of "closing the gap" — read carefully.** This increment closes the *provider-side* gap: the decparty's coupons no longer expire *unassigned*. It does **not** by itself guarantee CC reaches beneficiaries — after assignment each beneficiary holds its own coupon, which **its software agent must mint before the coupon expires** (§2, §4.3). That minting is a precondition, not a deliverable here. So "reward realized end-to-end" = this increment **plus** each beneficiary's agent minting.

Division of labour (settled with Robert, 2026-07-20): there is **no** shared mode/config template and **no** per-decparty mode selection. Mode B is a **one-shot** collection path Robert shipped separately (PR #256); this increment is the **Mode A automation**, which owns its own on-ledger config, `RewardSplitConfig` (§8). A decparty runs one approach or the other, never both at once (they'd compete for the same unassigned coupons).

## 2. Actor model — the three layers (the backbone of this design)

A Canton **party is not an active agent**. It is a passive on-chain representation. Every actor in this system decomposes into three layers, and keeping them distinct is what makes the design correct:

1. **Business entity (L1)** — makes the decisions (an org, or a consortium).
2. **Software agent (L2)** — acts on the entity's behalf: holds credentials, authenticates, submits transactions. A validator node or a wallet.
3. **Party id (L3)** — the indirect on-chain representation of the entity. It *is acted upon*; it does not act.

Mapping the parties in this design:

| | Business entity (L1) | Software agent (L2) | Party id (L3) |
|---|---|---|---|
| **decparty** (`governanceParty` / `cbtc-network`) | the member **consortium** (m-of-n orgs) | the **DecMan mesh** — each member's DecMan + participant, coordinated by threshold governance (*not* a wallet) | `cbtc-network::…` — a *decentralized*-namespace party |
| **beneficiary** (`cbtc-beneficiary`, operator) | the org that owns the beneficiary (BitSafe; the operator) | that org's **node + wallet** (e.g. the `cbtc-beneficiary-minter` agent) | `cbtc-beneficiary::…` — a normal party |
| **member node** running the automation | each member org | its **DecMan instance** (the automation) | its member party |

Structural consequence: a **normal** party is 1 entity / 1 agent / 1 party id; a **decparty** is N entities (threshold) / N coordinated agents / 1 party id. This is why no single wallet can serve a decparty, and it dictates the central design principle:

> **Never trust the agent (L2). Constrain it through the on-chain representation (L3), which encodes the entity's decision (L1).** Correctness and fairness must come from L3 artifacts that any L2 agent is forced to satisfy — not from an agent behaving well.

Every design choice below follows from this principle. **Writing rule for this doc:** a party id is never the grammatical subject of an action verb. An action is always done by an L2 *agent* (a node, a wallet, the automation) on behalf of an entity; "as the decparty" names the *capacity* an action is authorized in, not an actor. If a sentence says "the decparty does X," that's a bug in the sentence.

## 3. Scope

**In scope**
- A new `GovernableAction` DAML template `AssignRewardBeneficiaries` wrapping `RewardCoupon_AssignBeneficiaries`.
- A new on-ledger `RewardSplitConfig` template holding the resolved `[RewardBeneficiary]` split, plus a `SetRewardSplit` `GovernableAction` to set it through governance (§8).
- A new Rust background module in `crates/decman` — the automation — as **two roles**: a **proposer** (discovers coupons, proposes a batched assignment) and a **confirmer** (validates the proposal against on-ledger config and auto-confirms). Each member node runs an instance.
- Sourcing the effective beneficiary split from on-ledger (L3) governance config.
- Batching + TTL-watermark cadence; duplicate/all-or-nothing handling.
- The net-new **validation-gated auto-confirmation** engine (§5), built action-agnostic with `AssignRewardBeneficiaries` as its first and only enrolled policy.
- Tests: DAML action tests (mirroring splice reward-assignment tests), Rust unit tests, a devnet integration test.

**Out of scope (future increments)**
- Mode B (the one-shot collection path) — Robert (PR #256); a separate, non-runtime-selectable approach.
- Deterministic leader election + grace-window optimisation on the proposer side.
- Self-mint-as-a-service / grace-period sweeper for offline beneficiary agents.
- >20 beneficiaries (hierarchical reassignment / Merkle claim).
- Custom on-ledger split-enforcement contract (extended-phase trust hardening).
- Auto-confirmation for any action type other than `AssignRewardBeneficiaries`.

## 4. Assumptions & preconditions

Each is verified or flagged as a dependency.

1. **`governanceParty` == app-provider party.** `GovernableAction_Execute` is controlled by `governanceParty` and exercises `RewardCoupon_AssignBeneficiaries`, whose controller is the coupon's `provider`. This only authorizes if the decparty's governance party *is* the provider party. **Verified for CBTC:** `InstrumentConfiguration` has `provider == registrar == cbtc-network`, and DecMan governs as `cbtc-network`. If a decparty's governance party ever differs from its provider party, this design does not apply unchanged.
2. **Enablement — the decparty has a governance-configured `RewardSplitConfig`.** There is no mode selector. The automation runs for a decparty **iff** that decparty has exactly one on-ledger `RewardSplitConfig` (presence = on; absence = skip; >1 = refuse). Mode B (Robert's one-shot collection path, PR #256) is a separate approach that must not run on the same decparty — the two would compete for the same unassigned coupons.
3. **Each beneficiary has a minting agent (L2).** Mode A's premise: after assignment, each beneficiary's software agent mints its coupon before expiry. Building that agent is out of scope; the beneficiary must already have one (its own wallet automation, or a `MintingDelegation` to a node that runs the collect-rewards trigger). Without it, coupons expire at the beneficiary instead of the provider. **Verified for CBTC (devnet PQS):** the configured beneficiaries are wallet-capable *normal* parties, not decparties, so there is no recursion of the "can't run a wallet" problem — `cbtc-beneficiary` (80%) lives under the single-node attestor-1/bitsafe namespace and has a dedicated `cbtc-beneficiary-minter` L2 agent; the operator (20%, `auth0_…`) is a standard validator wallet user. Still untested end-to-end, because on devnet the coupons expired *unassigned at the provider* — nothing ever reached the beneficiaries to exercise their agents.
4. **Devnet has the live CIP-104 V2 stack.** Verified in PQS: `RewardCouponV2`, `splice-api-reward-assignment-v1:RewardCoupon` (the assign interface), and `MintingDelegation` are all deployed.
5. **`splice-api-reward-assignment-v1` is available as a DAML build dependency** for the new package, matching the version live on the target network. **Resolved:** vendored and verified against the target network — no longer an open assumption.

## 5. Why auto-confirmation, and what's net-new

**The core primitive we are creating.** Auto-confirmation does not exist in DecMan today. Confirming a governance action is currently a caller-initiated, `require_admin`-gated REST call (`confirm_action` → `execute_confirm_action`); a human triggers every confirmation. The one genuinely new mechanism in this increment is a background component that, per node, **validates a proposal against on-ledger policy and submits that node's confirmation automatically.** The *submission* plumbing already exists and is reused; the *automated, validated decision to confirm* is new.

**Why it's necessary.** `cbtc-network`'s authority is a threshold over its member agents (§2), so acting as the decparty means gathering ≥ threshold `GovernanceConfirmation` votes for each assign. Done by humans — an operator at each member org clicking "confirm" on every coupon batch — that voting could never keep pace with per-round coupons expiring in 36h. **So the automation automates the committee's voting.** The only model that is both **human-free** (no operator clicks) and **decentralized** is **validation-gated auto-confirmation**: each member node (an L2 agent) casts its confirmation *only if* the proposed beneficiary set matches the on-ledger (L3) governance split. This *is* the §2 principle made operational — agents are not trusted; the L3 split constrains them, and a node refuses to confirm anything that doesn't match.

**Two guardrails, because this is the highest-leverage and highest-risk piece:**
- **Opt-in per action type, default-deny.** Auto-confirmation is never blanket. It is an explicit allowlist of one: nodes auto-confirm `AssignRewardBeneficiaries` proposals that pass policy P, and nothing else. Every other action keeps the human in the loop. (Blanket auto-confirm would risk auto-approving a membership change or a treasury transfer.)
- **Non-custodial first.** Reward-assign is the safe first policy: it is non-custodial (CC never lands with the decparty) and the split is validated against governance. Custodial/transfer actions come later, behind caps encoded as on-ledger invariants.

**Build it action-agnostic.** Implement the engine as a policy interface (`is_confirmable(proposal, on_ledger_config) -> bool`) + a background loop reusing `execute_confirm_action`, and register exactly one policy now. Future actions (e.g. renewing an expiring `MintingDelegation`, or other non-custodial actions checkable against on-ledger policy) become new policies without widening today's blast radius.

**Two confirmation levels — don't conflate them.** What the engine automates is the **app-level** DecMan governance confirmations (`GovernanceConfirmation` contracts): the `GovernableAction_Execute` gate requires ≥ the *governance* threshold of these ("enough member confirmations to execute action" — an explicit DecMan threshold, per the code comment in `handlers/governance.rs`, distinct from the topology threshold). Separately, the **protocol-level** topology confirmation of the execute transaction (the decparty is hosted on N participants) happens automatically — each hosting participant validates and confirms per Canton's rules. The automation automates only the app-level layer.

## 6. Architecture overview

Two small pieces on top of existing DecMan machinery (propose/confirm/execute engine, per-node member credentials, ACS/PQS queries, `tokio::spawn` background tasks):

```
   Per-node automation (a member node's L2 agent — a new background task).
   Every member node runs BOTH roles on each tick:

   PROPOSER  (any node; duplicate proposals are safe — see §10)
     1. scan unassigned RewardCouponV2 (provider = decparty)
     2. read the configured split from the on-ledger (L3) RewardSplitConfig
     3. propose AssignRewardBeneficiaries(batch, split), then self-confirm
            │
            ▼
     ─── a GovernableAction proposal now exists on the ledger (L3) ───
            │
            ▼
   CONFIRMER (every node)
     1. see the pending proposal
     2. validate: proposed split == on-ledger (L3) config, coupons still unassigned,
        governanceParty == decparty
     3. if it matches, auto-submit this node's GovernanceConfirmation
            │
            ▼   app-level governance threshold of confirmations reached
   GovernableAction_Execute  (controller = the decparty)
            │   executeImpl
            ▼
   RewardCoupon_AssignBeneficiaries(coupons, split)
            │
            ▼
   one coupon per beneficiary  →  each beneficiary's own L2 agent mints it
```

**6.1 Existing pieces reused**
- Discovery: `get_governance_confirmations(party_id, threshold, …)` (`queries.rs:630`) lists `GovernableAction` proposals (via `fetch_proposal_infos`, generic over the interface) + their `GovernanceConfirmation`s and computes `can_execute = confirmation_count >= threshold`. The threshold here is the **governance** threshold read off the active `GovernanceRules` contract (the app-level DecMan threshold, resolved at `handlers/governance.rs:115–138`), **not** the topology threshold `get_party_threshold` returns — see §5's "two confirmation levels".
- Submission: `propose_action` / `execute_confirm_action` / `execute_action` (`handlers/governance.rs`) — the background loop calls the internal functions directly (not the `require_admin`-gated HTTP handlers), using the node's stored member credentials (`get_party_credentials`).
- Coupon/ACS reads: `get_contracts` (`queries.rs:251`).

**6.2 New plumbing required (not just DAML).** Proposing and confirming a *specific* action goes through a closed `ProposalType`/`ActionType` enum (`types.rs:351`) and per-variant `action_serializer` mappings. `SetProviderAppRewardBeneficiaries` has such a variant (`types.rs:627`, `action_serializer.rs:1163`); PR #248's `SetupMintingDelegation` is a second worked example of the exact same set of touch points. So the new action needs a matching `ProposalType::AssignRewardBeneficiaries` variant + `validate()` arm + serializer mapping + `ProposalPackage` + handler package-id mapping + `PackageConfig` field. Discovery is generic; representation/propose/confirm is not.

**6.3 Beneficiary minting (L2, not us).** Assignment produces one coupon per beneficiary. Each beneficiary's **software agent** then mints it (its wallet's collect-rewards trigger, or a `MintingDelegation`-based one). This is downstream of this increment (§4.3).

## 7. DAML: `AssignRewardBeneficiaries` GovernableAction

New module `Governance/Rewards/AssignRewardBeneficiaries.daml` (alongside PR #248's `Governance/Rewards/SetupMintingDelegation.daml`). Same `GovernableAction` structure as `SetProviderAppRewardBeneficiaries` / `SetupMintingDelegation`; only the payload and `executeImpl` body differ.

```haskell
template AssignRewardBeneficiaries
  with
    governanceParty : Party
    proposer : Party
    primaryCoupon : ContractId RewardCoupon          -- splice-api-reward-assignment-v1
    additionalCoupons : [ContractId RewardCoupon]     -- batch the rest in one tx
    newBeneficiaries : [RewardBeneficiary]            -- percentages sum to 1.0, ≤ maxNumNewBeneficiaries
  where
    signatory proposer
    observer governanceParty
    interface instance GovernableAction for AssignRewardBeneficiaries where
      view = GovernableActionView with
        governanceParty; proposer
        actionLabel = "AssignRewardBeneficiaries"
        description = "Assign governance-configured beneficiaries to reward coupons."
      executeImpl = do
        -- execute-time checks (not `ensure`: some state isn't knowable at create) —
        -- e.g. reject an empty beneficiary set; splice validates percentages/cap.
        _ <- exercise primaryCoupon RewardCoupon_AssignBeneficiaries with
               additionalCoupons
               newBeneficiaries
               extraArgs = emptyExtraArgs   -- empty; exact ctor per token-metadata API
        pure ()
```

Notes:
- `RewardCoupon_AssignBeneficiaries` requires each coupon to have **no** assigned beneficiary and caps beneficiaries at `maxNumNewBeneficiaries` (≤20). The DAML choice validates percentages (0,1] summing to 1.0.
- Because `GovernableAction_Execute` is controlled by `governanceParty` and the coupon choice's controller is the provider (= the same party), the governed execute carries the decparty's own authority — no extra delegation contract needed.
- DAR dependency: `splice-api-reward-assignment-v1` in `daml.yaml`.

## 8. Effective split & config source (`RewardSplitConfig`)

The split is an **L3 artifact that encodes the L1 decision** — this is where fairness is enforced, per §2. Contrast Mode B, where a fairness-relevant business decision — *which delegate* — stays off-chain, carried only as a free-text note the code can't act on. Mode A deliberately puts its fairness-relevant decision, the split, **on-chain**, which is exactly what lets it be *enforced* (the confirmer refuses anything that doesn't match) rather than *trusted*. The rationale for a particular split (why 20/80) remains an L1 fact and lives off-chain; only the split itself needs to be on-ledger for enforcement.

The split is read from a **Mode-A-owned on-ledger `RewardSplitConfig` contract** — a single authoritative source per decparty. It holds the resolved `[RewardBeneficiary { beneficiary; percentage }]` directly, with percentages summing to 1.0, and is set through governance via the `SetRewardSplit` `GovernableAction`. **The split is whatever governance configures** — there is no weight composition and no operator-cut derivation. If the DSO's (or an operator's) cut is part of the arrangement, it is just another configured `RewardBeneficiary` entry; the automation does not compute it. `RewardSplitConfig` stores percentages, not weights, so nothing is normalized at read time.

The **effective split** for CBTC today is, as an example configured value, **operator 20% + cbtc-beneficiary 80%** — stored directly as two `RewardBeneficiary` entries (0.2 and 0.8) in `RewardSplitConfig`. (The earlier `getBeneficiaries` weight-composition from utility-registry `InstrumentConfiguration` + `AppRewardConfiguration` is no longer used.)

## 9. Rust automation module (a member node's L2 agent)

New module `crates/decman/src/server/reward_automation/` (mirrors existing background-task style; registered via `tokio::spawn` in `start_server`, like the Canton sync loop). Cadence is a single global tick interval, `NodeConfig.reward_automation_interval_secs` (default 300s).

**Config / gating**
- Enablement is the **presence of exactly one on-ledger `RewardSplitConfig`** for the decparty (§8) — there is no mode gate. None ⇒ no-op (log and skip); more than one ⇒ refuse (log). The split read comes from that contract.
- **Not** gated on an active `FeaturedAppRight`: the FAR governs whether *new* coupons accrue, not whether *existing* unassigned coupons can be assigned. The automation acts on any live unassigned coupon whose `provider` is the decparty, even if the FAR has lapsed.
- Uses the node's existing member credentials; no new secrets.

**Proposer role** (runs on every node; duplicates are safe — see §10):
1. Query unassigned coupons: active `RewardCouponV2` where `provider = decparty` and `beneficiary = null`, ordered by `expiresAt` (minted/consumed coupons archive out of the ACS, so "active + beneficiary = null" suffices).
2. Select the batch by **TTL-watermark**: coupons whose age ≥ watermark (e.g. ~6h after creation, matching splice defaults) or approaching expiry, up to a conservative per-tx batch size (bounded by transaction/traffic size, *not* a fixed count — the ≤20 limit is beneficiaries per coupon, not coupons per batch). **Leave enough margin for the beneficiary's agent to mint afterward** — assigned coupons most likely inherit the original expiry, so assigning near the deadline could leave the beneficiary no time to mint. Cadence: a periodic tick (a few times/day), not per-round — coupons can be assigned any time before mint/expiry, minimizing tx cost.
3. Read the effective split (§8).
4. If no in-flight proposal already covers this batch, propose one `AssignRewardBeneficiaries`, then **self-confirm** its own proposal (so the proposal is visible on-ledger and counts toward threshold).

**Confirmer role** (runs on every node — the auto-confirmation engine, §5):
1. Discover pending `AssignRewardBeneficiaries` proposals via `get_governance_confirmations`.
2. **Validate** (policy P): proposed `newBeneficiaries` == effective on-ledger split (set + percentages compared with **exact `Decimal` equality**) AND target coupons are unassigned and belong to the decparty AND `governanceParty == decparty`.
3. If valid and not already confirmed by this node, auto-submit a `GovernanceConfirmation` via `execute_confirm_action`. If invalid, refuse (log; never confirm).
4. When `can_execute` (threshold reached), any node calls execute (first-wins; a second execute finds the proposal consumed and fails harmlessly).

**Input validation at the backend boundary.** Following PR #248's one gap (its `expires_at_micros` was unvalidated in `validate()`), `AssignRewardBeneficiaries`'s `validate()` must check inputs at the API boundary — non-empty coupon set, percentages in (0,1] summing to 1.0, ≤ `maxNumNewBeneficiaries` — so a direct API caller fails fast instead of wasting a governance round.

## 10. Coordination & idempotency

`RewardCoupon_AssignBeneficiaries` is **all-or-nothing and not idempotent**: a coupon MUST NOT already have a beneficiary, so assigning an already-assigned/archived coupon **fails the whole transaction**. The design turns this into a safety property:

- **Any node may propose** (no elected proposer → no single-node liveness dependency).
- **Duplicates/overlaps are safe, just wasteful.** If two proposals overlap, the first to execute assigns; the second fails harmlessly and changes nothing. Uncovered coupons are retried next tick. Correctness never depends on coordination.
- **Best-effort duplicate suppression (tx-cost only):** before proposing, a node skips coupons an in-flight proposal already covers (keyed by sorted coupon CIDs).
- **Confirm/execute:** a node confirms a proposal at most once; execute is first-wins; assigned coupons drop out of the next scan.
- Deterministic **leader + grace-window** is a follow-up that only cuts wasted proposals under large fleets — no correctness or liveness role, since any node proposing is already safe and live.

## 11. Error handling & edge cases

- **Coupon expires before assignment commits:** it leaves the ACS; a stale proposal's execute fails cleanly; next tick re-scans. Watermark leaves ample margin inside 36h.
- **Split mismatch:** confirmer refuses; the proposal never reaches threshold and becomes non-executable after `actionConfirmationTimeout` (below). Alert.
- **Transient config-view skew:** if nodes read the split at slightly different offsets (or it changes mid-flight), some confirmers reject a proposal built on a different view. This only *delays* — the proposal expires and the next tick re-proposes against converged config; it never mis-assigns.
- **Stale / never-executed proposals:** the design intent is to propose with a **bounded confirmation `expiresAt`** aligned to the batch's earliest coupon TTL, so unmet proposals auto-expire instead of lingering. **Not implemented in this increment (deferred, review finding R-4):** doing so would require changing the shared `GovernanceRules_ConfirmAction` choice, which is disproportionate for this increment; unmet proposals already become non-executable after the fixed `actionConfirmationTimeout`, so they do not execute stale. `get_governance_confirmations` already ignores expired confirmations for `can_execute`.
- **Confirmer input:** reconstruct the action (`ProposalType::AssignRewardBeneficiaries` — coupon CIDs + beneficiaries) from the on-ledger proposal payload, validate, then build the `ConfirmActionRequest`.
- **Config missing:** no-op with a clear log (no `RewardSplitConfig` ⇒ automation is off for that decparty).
- **Batch size:** bounded by transaction/traffic size, not a fixed count (§9, proposer step 2) — chunk conservatively; remainder next tick.
- **Beneficiary count:** capped by `maxNumNewBeneficiaries` (≤20); CBTC has 2, well within. >20 is out of scope (§3).
- **Configured split shape:** the split is already stored as `[RewardBeneficiary]` percentages summing to 1.0 in `RewardSplitConfig` (validated at set time via `SetRewardSplit`); there is no weight-to-percentage composition to perform at read time.
- **A node being down:** fewer proposers/confirmers. Others still propose; assignment proceeds while enough nodes are live to satisfy *both* the app-level governance threshold and the protocol-level topology threshold (§5) — the decparty's normal liveness bound.

## 12. Trust & security properties

Stated in the §2 frame — **fairness/correctness live in L3, never in L2 trust**:

- **No agent is trusted for the split.** Member-node agents (L2) auto-confirm only assignments that match the on-ledger (L3) governance split; a compromised/buggy proposer cannot exfiltrate or misdirect rewards, because its proposal can't reach threshold. Fairness is an L3 invariant.
- **Non-custodial.** CC never lands with the decparty; assignment routes each coupon to a beneficiary party, whose own agent mints it. The automation moves no funds.
- **No standing off-ledger secret.** The split lives on-ledger; the automation reads it, never a node config file.
- **Default-deny auto-confirmation.** Only `AssignRewardBeneficiaries` is enrolled; every other governance action still requires a human. Auto-confirmation's blast radius is exactly one non-custodial action type.

## 13. Testing

- **DAML:** action tests for `AssignRewardBeneficiaries` mirroring splice's reward-assignment tests (and PR #248's test shape): happy path, already-assigned coupon rejected (all-or-nothing), percentages sum to 1.0, beneficiary count ≤ `maxNumNewBeneficiaries`, batching via `additionalCoupons`, empty-set rejected at execute.
- **Rust unit:** `ProposalType::AssignRewardBeneficiaries` serialization round-trip (copy PR #248's `build_proposal_*_shape` test); `validate()` rejects empty set / bad percentages / >cap (the boundary test PR #248 lacked); batch selection (TTL-watermark, minting-margin); confirmer validation (accept correct split; reject mismatch on exact `Decimal` equality; reject on mid-flight config change); best-effort duplicate suppression.
- **Integration (devnet):** against live `cbtc-network` coupons — propose → auto-confirm across member nodes → execute → assert the beneficiaries' coupons appear with the expected split. (Beneficiary minting is a separate precondition, asserted only if those agents run.) Requires **multiple DecMan instances** (the decparty's member nodes) to exercise multi-node auto-confirm; extends the existing devnet IT phase (`test(integration): extend IT suite to devnet`), which may currently be single-instance.

## 14. Decisions (resolved with Robert, 2026-07-20)

1. **Config interface (§8):** "Option B" — a governance-config template, built **Mode-A-owned** as the on-ledger `RewardSplitConfig` contract, set via the `SetRewardSplit` `GovernableAction`. The automation reads the resolved `[RewardBeneficiary]` split directly from that single contract. **No `getBeneficiaries` composition and no operator-cut derivation** — "the split is whatever governance configures," so any DA/operator cut is just another configured entry.
2. **Package placement** of `AssignRewardBeneficiaries` (and `RewardSplitConfig` / `SetRewardSplit`): the `governance-rewards` package.
3. **Mode selector:** none. Mode A is built standalone; Mode B is a **one-shot** collection path (PR #256), not a runtime-selectable engine mode. There is no shared mode/config template and no per-decparty mode flag.

## 15. Milestone breakdown (proposed)

- **M1 — DAML action + tests:** `AssignRewardBeneficiaries` in the `governance-rewards` package + `daml.yaml` dep on `splice-api-reward-assignment-v1` + DAML tests. Reviewable in isolation.
- **M2 — Rust action plumbing:** `ProposalType::AssignRewardBeneficiaries` variant + `validate()` + `action_serializer` mapping + `ProposalPackage`/handler/`PackageConfig`; serialization + validation unit tests. Mirrors PR #248. Enables propose/confirm/execute from Rust.
- **M3 — auto-confirmation engine + automation:** the action-agnostic, default-deny auto-confirm engine (policy interface + background loop) with `AssignRewardBeneficiaries` as the sole enrolled policy; proposer + confirmer roles, effective-split read, TTL-watermark batching; Rust unit tests. (Bounded proposal `expiresAt` is **deferred** — see §11, review finding R-4; unmet proposals become non-executable after `actionConfirmationTimeout`.)
- **M4 — devnet integration:** end-to-end assignment of live `cbtc-network` coupons across member nodes; IT test.
- **M5 (future):** leader/grace-window; reconciliation/metrics; beneficiary self-mint enablement; additional auto-confirm policies (e.g. `MintingDelegation` renewal); Mainnet gating.
