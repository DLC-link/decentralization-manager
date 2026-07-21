# CIP-104 Coupon-Reassignment Automation (Mode A) — Design

**Date:** 2026-07-14 (rev. 2026-07-20 — pivoted to the delegation model)
**Status:** Design — delegation model
**Author:** Gyorgy Balazsi (with Claude)
**Design doc lives in:** `cip-104` · **Implementation target:** `decentralization-manager`

## In short

A consortium of orgs is represented on-chain by a single **decentralized party id** (`cbtc-network::1220…`). Under CIP-104 it earns rewards as `RewardCouponV2` coupon contracts; the party credited on each coupon is its **`provider`** field, which here is that decparty id.

But in that on-chain capacity the consortium has **no wallet** — it can act only through threshold governance, where the member orgs vote by hand. Coupons arrive every round and expire in ~36h, so there is no standing, per-round way to act on them, and the rewards expire uncollected.

This closes the gap with a **delegation**. The consortium votes **once**, through normal threshold governance, to create an on-ledger `CouponReassignmentDelegation` contract that carries the decparty's own authority and has the beneficiary **split baked into it**. Thereafter, **any single member's node can assign each round's coupons directly** — a plain ledger command, no per-round vote — but *only* to that baked-in split, because the split is fixed in the contract and the caller cannot override it.

The safety comes from the contract, not from trusting any node: the split is enforced **in DAML by construction (L3)**, so no node can misdirect rewards, and any one live member keeps assignment going (1-of-n liveness). One vote up front; no human clicks per round.

## Glossary

*Actor model (used throughout — see §2):*
- **Business entity (L1)** — the org, or group of orgs, that *makes decisions*. Off-chain.
- **Software agent (L2)** — the running software that *acts* for an entity: holds keys, submits transactions. A validator node or a wallet.
- **Party id (L3)** — the on-chain name (`foo::1220…`) that *represents* an entity, plus the on-ledger contracts that constrain what agents may do in its name. Passive: acted upon, never acting.

*Canton / parties:*
- **Validator node** — a Canton participant node; generic infrastructure any org runs. "Validator" alone says nothing about whose it is or in what role.
- **Consortium member** — one of the m-of-n orgs that jointly govern a decentralized party. Each wears **two hats**: a *decision-maker* co-governing the decparty (a business entity, L1), and a *node operator* whose participant + DecMan instance is a software agent (L2) — here, the node that runs the automation. Same org, two layers.
- **Decentralized party (decparty)** — one party id collectively controlled by a consortium via an m-of-n threshold (here, `cbtc-network`). No single member can act for it alone.
- **Threshold governance (propose → confirm → execute)** — DecMan's on-ledger flow by which a consortium acts as its decparty: a member proposes an action, members confirm (each submitting a `GovernanceConfirmation`), and once ≥ threshold have confirmed, any member executes. Human-driven, hence slow — which is why this design uses it **once** (to create the delegation), not per round.

*CIP-104 rewards:*
- **App provider (provider party)** — the party the DSO credits with app rewards. Here the decparty `cbtc-network` is its own provider.
- **`RewardCouponV2` / coupon** — a *minting right* (not CC) issued per round; must be redeemed before it expires (**TTL ≈ 36h**).
- **Beneficiary** — the party a coupon's value is assigned to; its agent mints the coupon.
- **Assign (`RewardCoupon_AssignBeneficiaries`)** — a choice the provider party controls that replaces an unassigned coupon with one coupon per beneficiary (each carrying its `beneficiary` + percentage); afterwards each beneficiary can mint its share.
- **Unassigned / reassignment (why this doc says both "assign" and "reassign").** A freshly-minted coupon is *unassigned* (`beneficiary = null`), which defaults its value to the **provider** — here the decparty, which cannot use it. This work **reassigns** the coupon away from that provider default to the governance-configured beneficiaries; that end-to-end routing is *coupon reassignment* (the product name). The on-ledger act that performs it is splice's `RewardCoupon_AssignBeneficiaries` choice, which *assigns beneficiaries* to a coupon. So throughout: **"assign" / "unassigned"** is the coupon-level, splice/ledger vocabulary (the `beneficiary` field), and **"reassignment"** is what this design does with it.
- **Self-mint** — a beneficiary's *agent* consuming its coupon to produce CC. Non-custodial: the CC lands with the beneficiary.
- **DSO** — the Splice super-validator collective that issues coupons / runs Amulet rules.
- **Effective split** — the resolved `{beneficiary → percentage}` set (for CBTC, an example configured value: operator 20% + `cbtc-beneficiary` 80%), baked into the `CouponReassignmentDelegation` (§8).

*This design:*
- **`CouponReassignmentDelegation` ("the delegation")** — the on-ledger contract this doc introduces (§7): signed by the decparty, carrying the fixed split, observed by the member parties who may trigger assignment. Created once by a governance vote.
- **Assigner** — a member party listed on the delegation that may exercise `Delegation_Assign`. `assigners = all members`, so any **one** live member suffices (1-of-n).
- **`Delegation_Assign`** — the delegation's nonconsuming choice: an assigner supplies the target coupons; the choice assigns them to the **baked-in** split. The caller never supplies the beneficiaries — that is the security boundary (§12).
- **Coupon-reassignment automation ("the automation")** — the per-node background process this doc specifies (§9): read the active delegation + the decparty's unassigned coupons, and exercise `Delegation_Assign` on a due batch. Any member node runs it; running on several is safe (§10).
- **Mode A / Mode B** — two alternative reward-distribution approaches, **not** runtime-selectable via a mode flag. A = assign & self-mint (this doc), via the delegation. B = collect coupons into the decparty via a separate `MintingDelegation`/`AcceptExternalPartySetup` collection path (a one-shot, shipped as PR #256). There is no shared mode selector; a decparty runs one approach or the other — never both, since they compete for the same unassigned coupons.

## 1. Context & goal

Under CIP-104, a consortium acting as an app provider — its **decparty id** is the coupon's `provider` — earns traffic-based app rewards as `RewardCouponV2` coupons. Each coupon is a *minting right* that must be redeemed (or have beneficiaries assigned) before it expires — default TTL **36h**.

The consortium cannot use the standard single-node wallet automation in its decparty capacity. Why is precise once you name the actors (§2): a wallet is a *single software agent* bound to a *single* entity's authority, but a decparty's authority is a **threshold over many agents**. So the members can act as the decparty only through slow threshold governance, with no standing, per-round way to act on the coupons. (You could call the reward engine a purpose-built, threshold-aware narrow subset of wallet functionality.)

Verified on devnet (PQS `pqs_cbtc`, snapshot round 51961 / 2026-07-09): there are 118 `RewardCouponV2` coupons (~43,303 CC) with `provider = cbtc-network` (the CBTC decparty), all with `beneficiary = null` — **0 collected, all expired unclaimed**, and **0 coupons ever assigned to the configured beneficiary**. This is the gap this work addresses.

**Goal:** the first production increment of the DecMan reward engine — **Mode A (assign & self-mint)**. The consortium votes **once** to create a `CouponReassignmentDelegation` that fixes the beneficiary split and carries the decparty's authority; thereafter per-node automation discovers the decparty's unassigned coupons and assigns them to that split via the delegation, so each beneficiary can mint its share. **The one governance vote replaces per-round voting entirely** — instead of an operator at each member org confirming every coupon batch (which could never keep pace with per-round coupons expiring in 36h), a single member's node assigns each batch directly, constrained by the delegation. Built to keep (code quality, tests), exercised on devnet.

**Scope of "closing the gap" — read carefully.** This increment closes the *provider-side* gap: the decparty's coupons no longer expire *unassigned*. It does **not** by itself guarantee CC reaches beneficiaries — after assignment each beneficiary holds its own coupon, which **its software agent must mint before the coupon expires** (§2, §4.3). That minting is a precondition, not a deliverable here. So "reward realized end-to-end" = this increment **plus** each beneficiary's agent minting.

Division of labour (settled in team coordination, 2026-07-20): there is **no** shared mode/config template and **no** per-decparty mode selection. Mode B is a **one-shot** collection path shipped separately (PR #256); this increment is the **Mode A automation**, whose configuration (the split) lives in the delegation contract it introduces (§8). A decparty runs one approach or the other, never both at once (they would compete for the same unassigned coupons).

## 2. Actor model — the three layers (the backbone of this design)

A Canton **party is not an active agent**. It is a passive on-chain representation. Every actor in this system decomposes into three layers, and keeping them distinct is what makes the design correct:

1. **Business entity (L1)** — makes the decisions (an org, or a consortium).
2. **Software agent (L2)** — acts on the entity's behalf: holds credentials, authenticates, submits transactions. A validator node or a wallet.
3. **Party id (L3)** — the indirect on-chain representation of the entity, together with the on-ledger contracts that bind what any agent may do in its name. It *is acted upon*; it does not act.

Mapping the parties in this design:

| | Business entity (L1) | Software agent (L2) | Party id (L3) |
|---|---|---|---|
| **decparty** (`governanceParty` / `cbtc-network`) | the member **consortium** (m-of-n orgs) | the **DecMan mesh** — each member's DecMan + participant, coordinated by threshold governance (*not* a wallet) | `cbtc-network::…` — a *decentralized*-namespace party |
| **beneficiary** (`cbtc-beneficiary`, operator) | the org that owns the beneficiary (BitSafe; the operator) | that org's **node + wallet** (e.g. the `cbtc-beneficiary-minter` agent) | `cbtc-beneficiary::…` — a normal party |
| **member node** running the automation | each member org | its **DecMan instance** (the automation) | its member party (an assigner on the delegation) |

Structural consequence: a **normal** party is 1 entity / 1 agent / 1 party id; a **decparty** is N entities (threshold) / N coordinated agents / 1 party id. This is why no single wallet can serve a decparty, and it dictates the central design principle:

> **Never trust the agent (L2). Constrain it through the on-chain representation (L3), which encodes the entity's decision (L1).** Correctness and fairness must come from L3 artifacts that any L2 agent is forced to satisfy — not from an agent behaving well.

The delegation model is the purest expression of this principle: the split is a field on an L3 contract, and the L2 assigner that triggers assignment **cannot supply or alter it** — the choice reads it from the contract. An agent can only trigger the pre-decided outcome, never choose a different one.

**Writing rule for this doc:** a party id is never the grammatical subject of an action verb. An action is always done by an L2 *agent* (a node, a wallet, the automation) on behalf of an entity; "as the decparty" names the *capacity* an action is authorized in, not an actor. If a sentence says "the decparty does X," that's a bug in the sentence.

## 3. Scope

**In scope**
- A new DAML template `CouponReassignmentDelegation` (in the `governance-rewards` package): signed by the decparty, holding the fixed `[RewardBeneficiary]` split and the assigner list, with a `nonconsuming` `Delegation_Assign` choice that assigns caller-supplied coupons to the baked-in split (§7).
- A `GovernableAction` to **create** the delegation through governance — `SetupCouponReassignmentDelegation` — and a governance path to **revoke** it (archive). Creating/replacing the delegation is the *only* action that goes through a threshold vote.
- A new Rust background module in `crates/decman` — the automation — that, per node, reads the active delegation + the decparty's unassigned coupons, selects a due batch, and exercises `Delegation_Assign` as a plain ledger command. Each member node runs an instance.
- Batching + TTL-watermark cadence with a minting margin; duplicate/all-or-nothing handling (§10).
- Tests: DAML tests for the delegation and its choice, Rust unit tests, a devnet integration test.

**Out of scope (future increments)**
- Mode B (the one-shot collection path, PR #256) — a separate, non-runtime-selectable approach owned by another workstream.
- Deterministic leader election + grace-window optimisation (cuts redundant assign attempts across a large fleet; no correctness/liveness role).
- Self-mint-as-a-service / grace-period sweeper for offline beneficiary agents.
- >20 beneficiaries (hierarchical reassignment / Merkle claim).
- Delegating any action other than coupon reassignment.

## 4. Assumptions & preconditions

Each is verified or flagged as a dependency.

1. **`governanceParty` == app-provider party.** The delegation is signed by `governanceParty`; `Delegation_Assign` exercises `RewardCoupon_AssignBeneficiaries`, whose controller is the coupon's `provider`. The nested exercise only authorizes if the decparty's governance party *is* the provider party. **Verified for CBTC:** `InstrumentConfiguration` has `provider == registrar == cbtc-network`, and DecMan governs as `cbtc-network`. If a decparty's governance party ever differs from its provider party, this design does not apply unchanged.
2. **Enablement — the decparty has exactly one active `CouponReassignmentDelegation`.** There is no mode selector. The automation runs for a decparty **iff** that decparty has exactly one active delegation (presence = on; absence = skip; >1 = refuse and alert — see §11). Mode B (the one-shot collection path, PR #256) is a separate approach that must not run on the same decparty — the two would compete for the same unassigned coupons.
3. **Each beneficiary has a minting agent (L2).** Mode A's premise: after assignment, each beneficiary's software agent mints its coupon before expiry. Building that agent is out of scope; the beneficiary must already have one (its own wallet automation, or a `MintingDelegation` to a node that runs the collect-rewards trigger). Without it, coupons expire at the beneficiary instead of the provider. **Verified for CBTC (devnet PQS):** the configured beneficiaries are wallet-capable *normal* parties, not decparties, so there is no recursion of the "can't run a wallet" problem — `cbtc-beneficiary` (80%) lives under the single-node attestor-1/bitsafe namespace and has a dedicated `cbtc-beneficiary-minter` L2 agent; the operator (20%, `auth0_…`) is a standard validator wallet user. Still untested end-to-end, because on devnet the coupons expired *unassigned at the provider* — nothing ever reached the beneficiaries to exercise their agents.
4. **Devnet has the live CIP-104 V2 stack.** Verified in PQS: `RewardCouponV2`, `splice-api-reward-assignment-v1:RewardCoupon` (the assign interface), and `MintingDelegation` are all deployed.
5. **`splice-api-reward-assignment-v1` is available as a DAML build dependency** for the new package, matching the version live on the target network. **Resolved:** vendored (`splice-api-reward-assignment-v1-1.0.0` + `splice-amulet-0.1.19`) and verified against the target network — no longer an open assumption.
6. **The decparty is co-hosted on every member's participant.** Each member's participant hosts both that member's party *and* the decparty — this is how DecMan members already see governance contracts and confirm as the decparty. Three things this design relies on follow from it: (a) the decparty's unassigned coupons are visible in each member node's ACS to scan; (b) exercising `Delegation_Assign` from a member node has the decparty's signatory authority available locally (no cross-participant delegation dance); (c) the topology-confirmation floor for a `Delegation_Assign` transaction is just the decparty's normal hosting threshold (§11). **Verified for CBTC:** `cbtc-network` is hosted on the attestor participants that also host the member parties (attestor-1/attestor-2 active).

## 5. Why a delegation

**The shape of the problem.** Acting as the decparty requires the decparty's authority, which is a threshold over the member agents (§2). There are two ways to give per-round coupon assignment that authority:

1. **Vote every round.** Treat each assignment as a governance action: propose, gather ≥ threshold confirmations, execute — once per coupon batch. This keeps the decision fresh but puts a recurring, time-critical quorum on the 36h critical path: assignment stalls whenever fewer than threshold member nodes confirm a given round in time.
2. **Vote once, into a contract.** Make the *recurring* decision — "assign each round's coupons to this split" — a single governance action that creates a standing on-ledger contract carrying the decparty's authority, with the split fixed inside it. Each round is then a plain command any one member can issue against that contract.

**This design takes (2): a delegation.** The recurring policy (the split) is decided once, by the full threshold; the recurring *mechanical act* (assign this batch) needs no further consensus, because the contract has already constrained it to exactly one outcome. This is (1) with the quorum moved off the per-round critical path and onto the one-time setup.

**Why it is safe.** The split is a field on the delegation, and `Delegation_Assign` uses that field — never a caller argument (§7, §12). So an assigner, acting with only its own single-member authority plus the decparty authority the delegation lends it, can assign coupons **only to the pre-voted split**. A compromised or buggy node cannot redirect rewards; the worst it can do is assign the correct split at an inconvenient time, or waste a transaction. Fairness is enforced in **L3 by construction**, not by trusting any L2 agent — the §2 principle, made structural rather than procedural.

**Why it is live.** `assigners = all members`, and the choice is `1-of-n` (any single assigner controls it — *not* a threshold). Assignment proceeds as long as **one** member node is up and the protocol-level topology threshold for the `Delegation_Assign` transaction is met (§11) — that topology confirmation is automatic, not a vote. There is no per-round governance quorum to miss.

**What changes the policy.** Because the split is baked in, changing it is not a config edit — it is a new governance vote that **archives the old delegation and creates a new one** (§8). There is no separate mutable split store to drift out of sync, and no window in which a stale split and a live delegation disagree.

## 6. Architecture overview

Two phases on top of existing DecMan machinery (propose/confirm/execute engine, per-node member credentials, ACS/PQS queries, `tokio::spawn` background tasks):

```
   ── PHASE 1: SETUP (once; the only threshold vote) ─────────────────
   A member proposes SetupCouponReassignmentDelegation(split, assigners);
   members confirm; any member executes.  Execute creates, with the
   decparty's authority:

        CouponReassignmentDelegation
          signatory decparty            (carries decparty authority)
          observer  assigners           (= all members)
          split     [RewardBeneficiary]  ← BAKED IN, the security boundary

   ── PHASE 2: PER-ROUND ASSIGN (every tick; no vote) ────────────────
   Per-node automation (a member node's L2 agent — a background task).
   Every member node runs this each tick; running on several is safe (§10):

     1. read the one active CouponReassignmentDelegation (skip if none)
     2. scan unassigned RewardCouponV2 (provider = decparty, beneficiary = null)
     3. select a due batch by TTL-watermark, leaving a minting margin
     4. exercise Delegation_Assign(assigner = self, primaryCoupon, additionalCoupons)
            │        controller = this node's member party (1-of-n)
            ▼
   Delegation_Assign  (nonconsuming; split is read from the contract)
            │   lends the decparty's signatory authority
            ▼
   RewardCoupon_AssignBeneficiaries(coupons, split)   ← controller = provider = decparty
            │
            ▼
   one coupon per beneficiary  →  each beneficiary's own L2 agent mints it
```

**6.1 Existing pieces reused**
- **One-time governance setup:** `submit_proposal` + `resolve_active_governance_rules` (governance rules cid + the **governance** threshold from the active `GovernanceRules` contract, `handlers/governance.rs:115–138` — *not* the topology threshold `get_party_threshold` returns) + `execute_confirm_action` + `execute_action`, driven by the node's stored member credentials (`get_party_credentials`). This is Phase 1 only; Phase 2 uses none of it.
- **Decoded ACS reads:** `active_created_records` (decoded `GetActiveContracts`, following the `fetch_proposal_infos` decode pattern) — used to read the active delegation and the unassigned coupons. (Not `query_contracts_by_template`, which is blob-only, nor `get_contracts`, which is metadata-only.) The coupon interface is `#splice-api-reward-assignment-v1:Splice.Api.RewardAssignmentV1:RewardCoupon`.
- **Plain command submission:** the per-round `Delegation_Assign` exercise is an ordinary ledger command from the executing node's member credentials — no governance round.

**6.2 New action plumbing (Phase 1 only).** Creating the delegation is a `GovernableAction`, so it needs the usual closed-enum touch points: a `ProposalType::SetupCouponReassignmentDelegation` variant + `validate()` arm + `action_serializer` mapping + `ProposalPackage` + handler package-id mapping + `PackageConfig` field. Revocation reuses the same machinery. `SetProviderAppRewardBeneficiaries` (`types.rs:627`, `action_serializer.rs:1163`) and PR #256's `SetupMintingDelegation` are worked examples of exactly these touch points. **The per-round assign needs none of this** — it is a plain exercise, not a governance action.

**6.3 Beneficiary minting (L2, not us).** Assignment produces one coupon per beneficiary. Each beneficiary's **software agent** then mints it (its wallet's collect-rewards trigger, or a `MintingDelegation`-based one). This is downstream of this increment (§4.3).

## 7. DAML: `CouponReassignmentDelegation`

New module `Governance/Rewards/CouponReassignmentDelegation.daml` in the `governance-rewards` package (alongside PR #256's `Governance/Rewards/SetupMintingDelegation.daml`). The delegation carries the decparty's authority and the fixed split; a nonconsuming choice lets any assigner assign coupons to that split.

```haskell
template CouponReassignmentDelegation
  with
    decparty   : Party                    -- = governanceParty = the coupons' provider
    assigners  : [Party]                  -- = all member parties; any one may assign (1-of-n)
    split      : [RewardBeneficiary]       -- BAKED IN; percentages in (0,1] summing to 1.0, ≤ maxNumNewBeneficiaries
  where
    signatory decparty
    observer assigners

    -- Per-round assignment. Non-consuming: one delegation serves every round
    -- until governance revokes/replaces it.
    nonconsuming choice Delegation_Assign : ()
      with
        assigner          : Party                       -- the submitting member; checked against `assigners`
        primaryCoupon     : ContractId RewardCoupon     -- splice-api-reward-assignment-v1
        additionalCoupons : [ContractId RewardCoupon]    -- batch the rest in one tx
      controller assigner                                -- the caller authorizes as `assigner`; 1-of-n, NOT a threshold
      do
        -- 1-of-n gate: the caller must be a listed assigner. Any single one suffices.
        assert (assigner `elem` assigners)
        -- The caller supplies ONLY the coupons + its own party. `newBeneficiaries` is read
        -- from the contract's `split` field — never from the caller. This is the security boundary.
        _ <- exercise primaryCoupon RewardCoupon_AssignBeneficiaries with
               additionalCoupons
               newBeneficiaries = split
               extraArgs = emptyExtraArgs   -- empty; exact ctor per token-metadata API
        pure ()

    -- Governance-only revoke (archive). Replacing the split = revoke + re-create (§8).
    choice Delegation_Revoke : ()
      controller decparty
      do pure ()
```

*(1-of-n is realized the standard DAML way: the choice takes the caller's own party as `assigner`, `controller assigner` requires that party to authorize the exercise, and `assert (assigner `elem` assigners)` restricts it to a listed member. Any single listed member suffices — it is a disjunction, not a threshold. The caller cannot escalate by passing another member's party: `controller assigner` means the submitter must actually authorize *as* that party.)*

Notes:
- **Authority flow.** Exercising `Delegation_Assign` makes available the union of the contract's signatory authority (`decparty`) and the choice controller's authority (one assigner). The nested `RewardCoupon_AssignBeneficiaries` requires the provider's authority (= `decparty`), which the delegation's signatory supplies. So a **single** member, with only its own authority as controller, can trigger assignment — but only to the baked-in split.
- **`RewardCoupon_AssignBeneficiaries`** requires each coupon to have **no** assigned beneficiary and caps beneficiaries at `maxNumNewBeneficiaries` (≤20); it validates percentages in (0,1] summing to 1.0. The split is validated once at delegation-create time too (see below), so a bad split can never be baked in.
- **Creation authority.** `signatory decparty` means the delegation can only be created with the decparty's authority — obtained through Phase-1 `GovernableAction_Execute` (controller = `governanceParty` = `decparty`). It cannot be created by any single member outside governance.
- **Revocation & replacement** are governance actions (`Delegation_Revoke`, controlled by `decparty`); the per-round choice never archives the delegation (nonconsuming), so one vote serves indefinitely.
- **Optional minting-margin guard.** `Delegation_Assign` may `assert` that each supplied coupon still has enough TTL left for a beneficiary to mint afterward (mirrors the Rust-side watermark, §9). Kept minimal; the Rust selector is the primary gate.
- **DAR dependency:** `splice-api-reward-assignment-v1` in `daml.yaml`.
- **Review gate:** `Delegation_Assign` is authority-carrying DAML — `newBeneficiaries = split` (not a caller argument) is the entire security model. It gets a security-focused review before merge (§13).

## 8. Effective split & where it lives (baked into the delegation)

The split is an **L3 artifact that encodes the L1 decision** — this is where fairness is enforced, per §2. Contrast Mode B, where a fairness-relevant business decision — *which delegate* — stays off-chain as a free-text note the code can't act on. Mode A puts its fairness-relevant decision, the split, **on-chain and immutable within a delegation**, which is exactly what lets it be *enforced by construction* (the choice reads it; a caller cannot override it) rather than *trusted*. The rationale for a particular split (why 20/80) remains an L1 fact and lives off-chain; only the split itself is on-ledger, for enforcement.

**Where it lives.** The split is a `[RewardBeneficiary { beneficiary; percentage }]` field on the `CouponReassignmentDelegation`, percentages summing to 1.0, validated once when the delegation is created (Rust `validate()` at the API boundary — §9 — and the DAML choice at execute). There is **no separate mutable split store**: baking the split into the delegation means there is nothing to keep in sync with it and no window in which a live delegation and a separate config disagree.

**The split is whatever governance configures** — there is no weight composition and no operator-cut derivation. If the DSO's (or an operator's) cut is part of the arrangement, it is just another configured `RewardBeneficiary` entry; the automation does not compute it. Percentages are stored directly, so nothing is normalized at read time.

**Changing the split (or the assigner set)** is a new governance vote: `Delegation_Revoke` the old delegation and `SetupCouponReassignmentDelegation` a new one. Where the governance flow lets one action archive-and-create in a single transaction, the swap is atomic (never zero or two active). Otherwise it is two separate votes, and the order determines the transient the automation must tolerate: revoke-then-create leaves a brief **zero**-delegation window (automation no-ops), create-then-revoke a brief **two**-delegation window (automation refuses — §11). Either way the automation never assigns to an ambiguous or stale split, because the old delegation's split is fixed and its archival removes it entirely. Membership changes are handled the same way, since `assigners` is likewise baked in.

The **effective split** for CBTC today is, as an example configured value, **operator 20% + cbtc-beneficiary 80%** — the two `RewardBeneficiary` entries (0.2 and 0.8) that would be baked into the delegation. (The earlier `getBeneficiaries` weight-composition from utility-registry `InstrumentConfiguration` + `AppRewardConfiguration` is not used.)

## 9. Rust automation module (a member node's L2 agent)

New module `crates/decman/src/server/reward_automation/` (mirrors existing background-task style; registered via `tokio::spawn` in `start_server`, like the Canton sync loop). Cadence is a single global tick interval, `NodeConfig.reward_automation_interval_secs` (env `DECPM_REWARD_AUTOMATION_INTERVAL_SECS`, default 300s).

**Config / gating**
- Enablement is the **presence of exactly one active `CouponReassignmentDelegation`** for the decparty (§8) — there is no mode gate. None ⇒ no-op (log and skip); more than one ⇒ refuse (log and alert, §11). The split is read from that contract, not from node config.
- **Not** gated on an active `FeaturedAppRight`: the FAR governs whether *new* coupons accrue, not whether *existing* unassigned coupons can be assigned. The automation acts on any live unassigned coupon whose `provider` is the decparty, even if the FAR has lapsed.
- Uses the node's existing member credentials; no new secrets, no off-ledger split.

**Per-tick loop** (runs on every member node; running on several is safe — see §10):
1. **Read the active delegation.** Load `CouponReassignmentDelegation` for the decparty via `active_created_records`. Zero ⇒ no-op; more than one ⇒ refuse + alert. This node's member party must appear in `assigners` (else it cannot assign — log and skip).
2. **Query unassigned coupons:** active `RewardCouponV2` where `provider = decparty` and `beneficiary = null`, ordered by `expiresAt`. (Minted/consumed coupons archive out of the ACS, so "active + beneficiary = null" suffices — no separate "already assigned" bookkeeping.)
3. **Select the batch** (`select_batch`) by **TTL-watermark**: coupons whose age ≥ watermark (e.g. ~6h after creation, matching splice defaults) or approaching expiry, up to a conservative per-tx batch size (bounded by transaction/traffic size, *not* a fixed count — the ≤20 limit is beneficiaries per coupon, not coupons per batch). **Leave enough margin for the beneficiary's agent to mint afterward** — assigned coupons most likely inherit the original expiry, so assigning near the deadline could leave the beneficiary no time to mint. Cadence: a periodic tick (a few times/day), not per-round — coupons can be assigned any time before mint/expiry, which minimizes tx cost.
4. **Assign the batch:** exercise `Delegation_Assign { assigner = this node's member party, primaryCoupon, additionalCoupons }` on the delegation, as a **plain ledger command** from this node's member credentials. No proposal, no confirmation, no execute round. On success the coupons leave the unassigned scan; on failure (e.g. a coupon already assigned by another node this tick) the whole exercise fails harmlessly and the remaining coupons are retried next tick (§10).

**Setup / revoke path (Phase 1, invoked out-of-band, not on the tick).** Creating or replacing the delegation goes through the reused governance flow (§6.1): `SetupCouponReassignmentDelegation` / revoke as `GovernableAction`s. `validate()` checks the split at the API boundary — non-empty beneficiary set, percentages in (0,1] summing to 1.0 (exact `DamlDecimal`), ≤ `maxNumNewBeneficiaries` (reuses `validate_reward_beneficiaries`) — so a bad split fails fast instead of wasting a governance round or baking in an invalid contract.

## 10. Coordination & idempotency

`RewardCoupon_AssignBeneficiaries` is **all-or-nothing and not idempotent**: a coupon MUST NOT already have a beneficiary, so assigning an already-assigned/archived coupon **fails the whole transaction**. The design turns this into a safety property:

- **Any assigner may assign** (no elected assigner → no single-node liveness dependency; 1-of-n).
- **Duplicate/overlapping assigns are safe, just wasteful.** If two nodes assign overlapping batches in the same tick, the first to commit assigns; the second finds those coupons already assigned (or archived) and its exercise fails harmlessly, changing nothing. Because assignment is all-or-nothing per *batch*, a single contended coupon wastes that node's whole batch for the tick — but every coupon it did not get to is still unassigned, so the next scan picks it up and the loop converges. Correctness never depends on coordination.
- **Best-effort duplicate suppression (tx-cost only):** a node may skip coupons it has just assigned in-process, but no cross-node coordination is required or attempted.
- **Assigned coupons drop out of the next scan** (they gain a beneficiary or archive), so the loop converges without external state.
- A deterministic **leader + grace-window** is a follow-up that only cuts wasted assign attempts under large fleets — no correctness or liveness role, since any node assigning is already safe and live.

## 11. Error handling & edge cases

- **Coupon expires before assignment commits:** it leaves the ACS; the `Delegation_Assign` exercise for a batch containing it fails cleanly (all-or-nothing); next tick re-scans the survivors. The watermark leaves ample margin inside 36h.
- **Coupon assigned by another node mid-tick:** same all-or-nothing failure; harmless; retried next tick.
- **No active delegation:** no-op with a clear log (automation is off for that decparty until governance creates one).
- **More than one active delegation:** refuse and alert (do **not** guess which split is authoritative). This occurs only transiently, during a create-then-revoke replacement (§8), or from a governance error; the automation stays off until exactly one is active.
- **This node not in `assigners`:** cannot assign; log and skip. Liveness still holds as long as one listed assigner's node is up.
- **Transient delegation-view skew during replacement:** while the split is being changed (§8), the automation sees zero or two delegations and no-ops/refuses rather than assigning to an ambiguous split. It never assigns to a stale split, because the old delegation's `split` is fixed and its archival removes it entirely.
- **Batch size:** bounded by transaction/traffic size, not a fixed count (§9, step 3) — chunk conservatively; remainder next tick.
- **Beneficiary count:** capped by `maxNumNewBeneficiaries` (≤20); CBTC has 2, well within. >20 is out of scope (§3).
- **A node being down:** fewer assigners available. Others still assign; assignment proceeds while **one** listed assigner's node is live *and* the protocol-level topology threshold for the `Delegation_Assign` transaction is met (the decparty is hosted on N participants; each hosting participant confirms per Canton's rules). That topology bound is the decparty's normal liveness floor and is unchanged by this design — but note it is a *protocol-level* threshold on the hosting participants, distinct from the *governance* threshold that only Phase-1 setup needs.

## 12. Trust & security properties

Stated in the §2 frame — **fairness/correctness live in L3, never in L2 trust**:

- **No agent is trusted for the split.** The split is a field on the delegation and `Delegation_Assign` reads it directly (`newBeneficiaries = split`); the caller supplies only coupon ids. A compromised or buggy assigner cannot exfiltrate or misdirect rewards — the worst it can do is assign the correct split at an inconvenient time or waste a transaction. Fairness is an L3 invariant enforced **by construction**, not a runtime check any node could skip.
- **Setup takes the full threshold; only setup does.** The delegation can only be created (or replaced/revoked) by a `GovernableAction` reaching the governance threshold. A single member cannot create, alter, or widen a delegation. Per-round assignment needs no vote precisely *because* the one-time vote already fixed the only degree of freedom.
- **1-of-n liveness, m-of-n control.** Any single assigner's node can trigger assignment — the recurring per-round *governance* quorum is gone; only the decparty's normal, automatic topology-confirmation floor remains (§11), which every action already pays. Changing *what* gets assigned still takes the full committee. Control and liveness are decoupled — the point of the design.
- **Non-custodial.** CC never lands with the decparty; assignment routes each coupon to a beneficiary party, whose own agent mints it. The automation moves no funds.
- **No standing off-ledger secret.** The split lives on-ledger; the automation reads it, never a node config file.
- **The choice is the security boundary.** `Delegation_Assign` is authority-carrying DAML and gets a security-focused review (§13); its correctness — that `newBeneficiaries` is the contract field, never a caller argument, and the controller is a 1-of-n assigner rather than an open party — *is* the trust model.

## 13. Testing

- **DAML** (for `CouponReassignmentDelegation`): happy path (an assigner assigns a batch to the baked-in split); **caller cannot alter the split** (the choice takes no beneficiary argument — asserted structurally, and the produced coupons carry exactly `split`); a non-assigner party cannot exercise `Delegation_Assign`; already-assigned coupon rejected (all-or-nothing); batching via `additionalCoupons`; the delegation can only be created with the decparty's authority (not a lone member); `Delegation_Revoke` archives it and is controlled by the decparty; nonconsuming — the delegation survives repeated assigns. Split validation (percentages in (0,1] summing to 1.0, ≤ cap, non-empty) at create. Mirrors splice's reward-assignment test shape.
- **Rust unit:** `ProposalType::SetupCouponReassignmentDelegation` serialization round-trip (copy the `build_proposal_*_shape` test shape); `validate()` rejects empty set / bad percentages / >cap (exact `DamlDecimal`, via `validate_reward_beneficiaries`); batch selection (`select_batch` — TTL-watermark, minting-margin); the per-tick gating (zero delegations → no-op; two → refuse; node-not-assigner → skip).
- **Integration (devnet):** against live `cbtc-network` coupons — create the delegation via governance (one vote), then a **single** member node exercises `Delegation_Assign` and we assert the beneficiaries' coupons appear with the expected split. (Beneficiary minting is a separate precondition, asserted only if those agents run.) This requires only **one** member node to assign — there is no per-round multi-node confirmation to exercise. Gated behind `DECPM_IT_REWARD`; extends the existing devnet IT phase. Note (devnet reality): `cbtc-network`'s coupons are currently swept to 0 by the Mode-B collection path, so the IT needs that paused (§ handover / team coordination).

## 14. Decisions (resolved in team coordination, 2026-07-20)

1. **Model:** the **delegation model** — one governance vote creates a `CouponReassignmentDelegation` with the split baked in; per-round assignment is a plain 1-of-n exercise, no per-round vote. The split is enforced in L3 DAML by construction, and the recurring quorum is off the 36h critical path — only the one-time setup needs the governance threshold.
2. **Config source (§8):** "Option B" — a governance-config artifact — realized as the split **baked into the delegation contract**, not a separate mutable split store. **No `getBeneficiaries` composition and no operator-cut derivation** — "the split is whatever governance configures," so any DA/operator cut is just another configured entry.
3. **Package placement** of `CouponReassignmentDelegation` (and the `SetupCouponReassignmentDelegation` / revoke actions): the `governance-rewards` package.
4. **Mode selector:** none. Mode A is built standalone; Mode B is a **one-shot** collection path (PR #256), not a runtime-selectable engine mode. There is no shared mode/config template and no per-decparty mode flag; the two must not run on the same decparty.

## 15. Milestone breakdown (proposed)

- **M1 — DAML delegation + tests:** `CouponReassignmentDelegation` (template + `Delegation_Assign` + `Delegation_Revoke`) in the `governance-rewards` package + `daml.yaml` dep on `splice-api-reward-assignment-v1` + the DAML tests in §13. Reviewable in isolation; the `Delegation_Assign` security review lands here.
- **M2 — Rust setup plumbing:** `ProposalType::SetupCouponReassignmentDelegation` (+ revoke) variant + `validate()` + `action_serializer` mapping + `ProposalPackage`/handler/`PackageConfig`; serialization + validation unit tests. Mirrors PR #256. Enables creating/replacing the delegation through governance.
- **M3 — per-round automation:** the background loop — read active delegation, scan unassigned coupons, `select_batch` (TTL-watermark + minting-margin), exercise `Delegation_Assign` as a plain command; per-tick gating; Rust unit tests.
- **M4 — devnet integration:** end-to-end — governance creates the delegation, a single member node assigns live `cbtc-network` coupons; IT test (needs Mode-B collection paused on `cbtc-network`).
- **M5 (future):** leader/grace-window; reconciliation/metrics; beneficiary self-mint enablement; Mainnet gating.
