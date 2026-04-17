# Token Issuance in `canton-vault`

Background reading on how the [DLC-link/canton-vault](https://github.com/DLC-link/canton-vault) project mints and burns its own token (the vault share), using the Splice `BurnMintFactory` interface and the Utility-Registry `AllocationFactory` implementation. Links below point at `main` on GitHub (canton-vault and its `splice` submodule at `hyperledger-labs/splice`).

## Core concepts

- **Token** — in the Splice / CIP-56 model, a token is a set of `Holding` contracts that share an `InstrumentId`. There is no single "supply" contract; supply is the sum of live `Holding`s for that instrument.
- **`InstrumentId`** — a record `{ admin : Party, id : Text }` defined in `Splice.Api.Token.HoldingV1`. Two instruments are the same token iff both fields match.
- **Instrument admin** — the `Party` inside the `InstrumentId`. The authoritative issuer of that instrument. Every mint or burn of the instrument requires the admin's authority as controller.
- **Holding** — a `Splice.Api.Token.HoldingV1.Holding` interface contract. An instance represents one (owner, amount, instrumentId) entry. Created only by mint, archived only by burn or by being spent through a transfer.
- **`BurnMintFactory` (interface)** — the Splice interface (in `splice-api-token-burn-mint-v1`) that turns mint/burn into a single choice call. One choice `BurnMintFactory_BurnMint` covers pure mints, pure burns, and atomic burn+mint.
- **`TransferFactory` (interface)** — the Splice interface (in `splice-api-token-transfer-instruction-v1`) that *initiates* a transfer. Its one business choice `TransferFactory_Transfer` takes a `Transfer` record (sender, receiver, amount, instrument, input holding cids) and produces a `TransferInstruction`. Orthogonal to `BurnMintFactory` — transfers don't change supply, only custody.
- **`TransferInstruction` (interface)** — the Splice interface (same package) representing *a single pending transfer* created by `TransferFactory_Transfer`. The sender's holdings are already locked on it. Key choice: `TransferInstruction_Accept` (receiver completes the transfer — sender's holdings move to receiver). Also `TransferInstruction_Withdraw` (sender cancels) and `TransferInstruction_Reject` (receiver refuses). Canton-vault uses the two-step pattern throughout: the user creates a pending instruction and the vault manager accepts it.
- **`AllocationFactory` (template)** — the Utility-Registry template (in `utility-registry-app-v0`) that implements both `BurnMintFactory` and `TransferFactory` as a single on-ledger contract. One ContractId, two interfaces: pass it as `ContractId BurnMintFactory` to mint/burn, pass the same id as `ContractId TransferFactory` to create a transfer instruction.
- **Registrar service** — another Utility-Registry contract whose choices create instrument configurations and allocation factories for a given registrar party.
- **Vault share / vault asset** — two distinct instruments related to the vault. The **asset** (a.k.a. the *underlying*) is whatever token users deposit in exchange for a vault share (e.g. CBTC) — an external instrument administered by some other party. The **share** is the vault's *own* token, issued to depositors as a claim on the pool of underlying assets; the vault manager is its admin. Only the share is issued by the vault; the asset is just held.
- **Vault manager** — the party that runs the vault. In canton-vault the vault manager wears three roles at once: instrument admin for the share token, registrar that owns the `AllocationFactory`, and signatory on the `Vault` contract. All three must be the same party for the authority chain to close.

## Why — two independent concerns, bundled by design

Each deposit and withdrawal in canton-vault does **two structurally independent things** in the same Daml transaction:

1. **Issuance** — mint or burn share tokens. A supply change on the share instrument, expressed as a call to `BurnMintFactory_BurnMint`.
2. **Swap** — exchange the underlying asset for the share (deposit) or vice versa (withdrawal). A custody change, expressed as `TransferInstruction_Accept` on asset-side instructions (deposit) or share-side instructions (withdrawal).

The two are independent at the interface level: different Splice interfaces (`BurnMintFactory` vs. `TransferFactory` / `TransferInstruction`), different `exercise` calls, no data-flow dependency on the deposit side, and only a soft dependency on the withdrawal side (the burn consumes holdings the swap-in step happens to deliver to `vaultManager`). Canton-vault bundles them in one transaction to enforce the economic invariant that a share exists iff a matching asset deposit occurred — not because the interfaces require it.

Concretely, if you deleted the swap step from each path:

- **Deposit, mint stands alone.** `BurnMintFactory_BurnMint` with empty inputs and a user-targeted output just creates a share holding. The authority it needs (`vaultManager :: [user]`) comes from the `DepositRequest` signatories, not from the swap. The mint succeeds; the assets never move. Technically valid, economically nonsense.
- **Withdrawal, burn degenerates.** The burn's `inputHoldingCids = receivedHoldingCids` comes directly from the swap's accept-results list. Without the swap, that list is empty; `BurnMintFactory_BurnMint` with empty inputs and empty outputs is a valid no-op. You *could* supply other share holdings, but only ones already owned by `vaultManager` — and in canton-vault's flow those only come into existence via the withdrawal swap. Practically unreachable.

The takeaway: `BurnMintFactory_BurnMint` is a self-contained primitive. It does not require any particular custody flow around it. Canton-vault pairs it with a user-driven asset swap; nothing in the interface demands that pairing.

## How — the `BurnMintFactory_BurnMint` primitive

Defined in [`BurnMintV1.daml`](https://github.com/hyperledger-labs/splice/blob/main/daml/splice-api-token-burn-mint-v1/daml/Splice/Api/Token/BurnMintV1.daml):

- **Viewtype** `BurnMintFactoryView { admin : Party, meta : Metadata }`.
- **One business choice** `BurnMintFactory_BurnMint`, with arguments:
  - `expectedAdmin : Party` — caller declares which admin it expects. The implementation must fail if this does not match `(view this).admin`. Defends against malicious substitute factories.
  - `instrumentId : InstrumentId` — all inputs and outputs must match this.
  - `inputHoldingCids : [ContractId Holding]` — holdings to archive (burn). Empty for pure mint.
  - `outputs : [BurnMintOutput { owner, amount, context }]` — new holdings to create. Empty for pure burn.
  - `extraActors : [Party]` — additional controllers on top of the admin.
  - `extraArgs : ExtraArgs` — metadata and registry-specific context.
- **Controller** `(view this).admin :: extraActors`. The admin is always required. Implementations may demand additional actors — typically the owner of the newly-minted holding (for mint) or no one else (for burn).
- **Result** `{ outputCids : [ContractId Holding] }` — the created holdings in the same order as `outputs`.

The same choice covers three use patterns:

| Pattern | `inputHoldingCids` | `outputs` |
|---|---|---|
| Pure mint | `[]` | non-empty |
| Pure burn | non-empty | `[]` |
| Atomic burn+mint | non-empty | non-empty |

What goes in `extraActors` is implementation-specific — canton-vault's choices are explained in the corresponding "What" sections below.

Canton-vault uses only the pure-mint and pure-burn patterns.

## What — how canton-vault wires up token issuance

There is no bespoke "vault balance" contract — a user's share position is simply their `Holding`s of the share instrument, like any other Splice token. Everything described below (deposit queues, yield accounting, FAR markers, pause gates) is scaffolding on top of the two primitives.

### Package layout

- [`vault-daml/core`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core) — package `bitsafe-vault-v0-rc8`. Contains `Vault`, `DepositRequest`, `WithdrawRequest`, `VaultRules`, `VaultProcessor`, `VaultProcessorRules`, `YieldEpoch`, `Types`. Depends on the Splice token APIs (`holding-v1`, `transfer-instruction-v1`, `burn-mint-v1`, `metadata-v1`, `featured-app-v1`) and on the Utility-Registry app DAR.
- [`vault-daml/governance`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/governance) — package `bitsafe-vault-governance-v0-rc8`. Wraps `VaultRules` choices behind a multi-sig governance committee (`VaultGovernanceRules`). Not directly involved in mint/burn, but it owns the setup path that provisions the `AllocationFactory`.

### Instrument identity

The share's `InstrumentId` is built from the vault manager and the share symbol in [`VaultRules.daml:63-68`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core/daml/BitsafeVault/VaultRules.daml#L63-L68):

```daml
let config = VaultConfig with
      shareInstrumentId = InstrumentId with
        admin = vaultManager
        id = shareSymbol
```

Immediately afterwards the same choice registers the share instrument with the Utility Registry via [`RegistrarService_CreateInstrumentConfiguration`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core/daml/BitsafeVault/VaultRules.daml#L70-L74). That registration is what makes the registry aware of the instrument; the `admin = vaultManager` assignment is what grants the vault manager mint/burn authority over it. The asset instrument (what users deposit) is just a field on the `Vault` contract — it is *not* created by the vault; the vault is only a holder.

### Factory deployment chain

The `AllocationFactory` that the vault uses for mint/burn is provisioned up front, before any vault exists, through a governance-driven Utility-Registry onboarding in [`VaultGovernance.daml:469-514`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/governance/daml/BitsafeVault/VaultGovernance.daml#L469-L514):

1. `ProviderService_CreateProviderConfiguration` — register the vault manager as a provider.
2. `ProviderService_AcceptRegistrarServiceRequest` — obtain a `RegistrarService` with `registrar = vaultManager`.
3. `RegistrarService_CreateAllocationFactory` → yields the `ContractId BurnMintFactory` (the `AllocationFactory` implements this interface).
4. `RegistrarService_CreateTransferRule` — out of scope for issuance.

The resulting factory cid is then **passed as a parameter** to `VaultRules_DeployVault` (field `allocationFactoryCid : ContractId BurnMintFactory`), which stores it in the new `Vault` contract at [`VaultRules.daml:76-86`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core/daml/BitsafeVault/VaultRules.daml#L76-L86); the `DepositRequest` and `WithdrawRequest` contracts then carry it forward as an ordinary field.

The three roles the vault manager wears (instrument admin, registrar, vault signatory) must be the same party for mint/burn to succeed. Enforcement is split across two layers:

- **Admin = vault signatory is a construction invariant.** `VaultRules_DeployVault` builds `shareInstrumentId.admin` from its `vaultManager` argument *and* creates the `Vault` contract with `signatory vaultManager` in the same body. Both come from the same variable, so they cannot diverge. Nothing is "checked" — they are assigned from the same source.
- **Admin = registrar is enforced at runtime by the factory interface, not at vault-deploy time.** The `allocationFactoryCid` is just a parameter of `VaultRules_DeployVault`; canton-vault doesn't inspect the factory's admin there. The check happens at every mint/burn, via two mechanisms that both must hold: (1) `BurnMintFactory_BurnMint` validates `expectedAdmin == (view this).admin`, and canton-vault always passes `expectedAdmin = vaultManager`; (2) the choice's controller is `(view this).admin :: extraActors`, so the exercise needs that admin's authority in scope — inside `DepositRequest_Process` / `WithdrawRequest_Process` the only governance-party authority live is `vaultManager`'s. A mismatched factory fails the first deposit or withdrawal, not the deploy.

Operationally, alignment comes from the governance onboarding path (`VaultGovernanceRules_SetupUtility`): it provisions a `RegistrarService` with `registrar = vaultManager` and then calls `RegistrarService_CreateAllocationFactory` on it, so the factory's admin ends up as `vaultManager` by construction of the setup flow. That is convention, not an on-chain invariant.

### Issuance — the mint and burn calls

Both deposit and withdrawal reduce, on the issuance side, to a single call to `BurnMintFactory_BurnMint` using the pre-provisioned `allocationFactoryCid` stored in the `Vault` contract.

**Mint call (deposit)** — located in [`DepositRequest_Process`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core/daml/BitsafeVault/Vault.daml#L328-L356):

```daml
exercise allocationFactoryCid BurnMintFactory_BurnMint with
  expectedAdmin = vaultManager
  instrumentId = shareInstrumentId
  inputHoldingCids = []
  outputs = [BurnMintOutput with
    owner = user
    amount = sharesToMint
    context = emptyChoiceContext]
  extraActors = [user]
  extraArgs = ...
```

Controller evaluates to `vaultManager :: [user]`. `sharesToMint` is computed by the backend from the current share value stored in `YieldEpoch` (first deposit: 1 share per asset unit; afterwards `assets / (totalAssets / totalShares)`), see [`YieldEpoch.daml:43-64`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core/daml/BitsafeVault/YieldEpoch.daml#L43-L64).

**Burn call (withdrawal)** — located in [`WithdrawRequest_Process`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core/daml/BitsafeVault/Vault.daml#L439-L445):

```daml
exercise allocationFactoryCid BurnMintFactory_BurnMint with
  expectedAdmin = vaultManager
  instrumentId = shareInstrumentId
  inputHoldingCids = receivedHoldingCids
  outputs = []
  extraActors = []
  extraArgs = ...
```

Controller reduces to `vaultManager` alone. Symmetric with the mint: the `extraActors = [user]` on the mint side includes the new-holding owner as an extra actor; the `extraActors = []` on the burn side reflects that the input holdings' owner, after the swap has moved them, is `vaultManager` — who is already the admin — so no extras are needed. `receivedHoldingCids` is the list of share-holdings that the burn consumes; how that list gets populated is the swap's job, described below.

**Authority binding for both calls.** The `BurnMintFactory_BurnMint` controller `(view this).admin :: extraActors` requires `vaultManager`'s authority. Inside `DepositRequest_Process` and `WithdrawRequest_Process`, that authority is live because exercising a choice on a contract makes the contract's signatory authorities available to the choice body — and the `DepositRequest` / `WithdrawRequest` contracts are both signed by `vaultManager` (and `user`). The backend (`vaultBackendSignatory`) is the submitter, but contributes only its controller authority; the issuance itself rides on `vaultManager`'s signatory authority inherited from the request contract.

The non-obvious detail on the mint side is `extraActors = [user]`: the vault includes the new-holding owner as a controller. The `BurnMintFactory` interface allows but does not mandate this; the enforcement that it is required lives inside the `AllocationFactory` implementation in `utility-registry-app`.

### Swap — the asset ↔ share exchange around each issuance call

Each issuance call is paired with a custody change expressed through Splice `TransferInstruction`s. These are the ledger events that **trigger** the issuance: neither side of the swap can occur without the other in the same transaction, so a user cannot obtain a share without the asset actually moving, and the vault manager cannot mint a share without a matching asset arrival.

**Deposit swap** (asset in, share out) — all in the same transaction:

1. **Off-request, one step upstream:** the user pre-creates `TransferInstruction`s (via a `TransferFactory_Transfer` with themselves as sender, `vaultManager` as receiver, the asset instrument, and their asset holdings as inputs). These instructions lock the user's assets.
2. **`Vault_Deposit`** (controller `user`): the user passes those `TransferInstruction` cids. `Vault_Deposit` validates them (sender, receiver, instrument, total amount) and creates a `DepositRequest` with signatories `[user, vaultManager]`.
3. **`DepositRequest_Process`** (controller `vaultBackendSignatory`, called by `VaultProcessor` during batch processing): in one transaction —
    - Exercises `TransferInstruction_Accept` on each deposit instruction → asset holdings move from user to `vaultManager`.
    - Runs the mint call above → a share holding appears with `owner = user`.

The asset-in and share-out are in the same choice body; a ledger-validation failure on either rolls back both.

**Withdrawal swap** (share in, asset out) — split across transactions:

1. **Off-request:** the user pre-creates share-side `TransferInstruction`s (sender = user, receiver = `vaultManager`, instrument = share, input holdings = user's shares). Each instruction **must** carry the metadata key `com.bitsafe.vault/withdrawal` to distinguish a withdrawal-intent transfer from an arbitrary share transfer to the vault manager ([`Vault.daml:162-163`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core/daml/BitsafeVault/Vault.daml#L162-L163)).
2. **`Vault_Withdraw`** (controller `user`): validates and creates a `WithdrawRequest` with signatories `[user, vaultManager]`.
3. **`WithdrawRequest_Process`** (controller `vaultBackendSignatory`, called by `VaultProcessor`): in one transaction —
    - Exercises `TransferInstruction_Accept` on each share instruction → share holdings move from user to `vaultManager`. The receiver holdings' cids are collected.
    - Runs the burn call above, consuming those share holdings → supply drops.
4. **Asset payout (separate transaction)**: `VaultProcessor_ProcessWithdrawals` creates an asset-side `TransferInstruction` from `vaultManager` back to `user` using the same `AllocationFactory` via its `TransferFactory` interface. The user accepts it later in yet another transaction to receive the asset.

So on the deposit side the full swap (asset-in + share-out) is atomic. On the withdrawal side only the share-in + burn is atomic; the asset-out leg is deferred. The user's risk on withdrawal is that their shares are already burnt before the asset-side transfer instruction is accepted; the mitigation is protocol discipline (backend creates the asset TI promptly).

### Supply accounting

There is no supply contract for the share instrument. Ground-truth supply is the sum of live `Holding`s. A separate [`YieldEpoch`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core/daml/BitsafeVault/YieldEpoch.daml) contract holds the running totals `totalSharesIssued` and `totalAssets` for the purpose of computing share value on future deposits. `YieldEpoch_ApplyUpdate` is invoked by the outer batch choices — `VaultProcessor_ProcessDeposits` ([`:529-537`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core/daml/BitsafeVault/VaultProcessor.daml#L529-L537)) and `VaultProcessor_ProcessWithdrawals` ([`:585-592`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/core/daml/BitsafeVault/VaultProcessor.daml#L585-L592)) — in the same atomic transaction as the issuance call. As long as every mint and burn goes through the vault's deposit / withdrawal flow, `YieldEpoch` cannot diverge from the holdings because both move together. (`vaultManager`, as instrument admin, could in principle call `BurnMintFactory_BurnMint` directly outside the vault; such out-of-band issuance would not update `YieldEpoch` and would break this invariant. Canton-vault's design trusts `vaultManager` not to do that.)

### The decentralized `vaultManager` plays no role in issuance

Canton-vault's `vaultManager` is a decentralized party — [`VaultGovernanceRules`](https://github.com/DLC-link/canton-vault/blob/main/vault-daml/governance/daml/BitsafeVault/VaultGovernance.daml#L289-L305) is a multi-sig committee whose members confirm actions on its behalf — but the committee only votes on **vault-lifecycle decisions** (deploy, pause, update limits, update backend signatory), never on individual mints or burns. The reason is structural: `vaultManager`'s issuance authority is captured *once* as the signatory of the `Vault` contract at vault-deploy time, and from then on every `DepositRequest` and `WithdrawRequest` inherits it automatically. Replace `vaultManager` with a single-party operator and drop `VaultGovernanceRules` entirely, and the mint and burn flows would work identically — decentralization is a property the vault's governance layer uses, not one the issuance layer relies on.

## Net summary

Canton-vault delegates all issuance primitives to Splice (`InstrumentId`, `Holding`, `BurnMintFactory`) and all custody primitives to Splice as well (`TransferFactory`, `TransferInstruction`). Its contribution is to **bundle** issuance and swap into a single atomic operation per user action:

- **Setup:** a `vaultManager` party is set up as instrument admin and registrar; a `Vault` contract captures the vaultManager's signatory authority and stores the `AllocationFactory` cid for use in every subsequent issuance.
- **Issuance:** two call sites — one in `DepositRequest_Process` (mint: empty inputs, non-empty outputs, user as extraActor) and one in `WithdrawRequest_Process` (burn: non-empty inputs, empty outputs, no extraActors) — each executing a single `BurnMintFactory_BurnMint`.
- **Swap:** each issuance is paired with a CIP-56 `TransferInstruction_Accept` step on the user's pre-locked assets (deposit) or shares (withdrawal). The deposit swap is atomic with the mint; the withdrawal swap has the share-in leg atomic with the burn but defers the asset-out leg to a separate transaction.
- **Bookkeeping:** `YieldEpoch` tracks supply in the same atomic transaction as each issuance.

Canton-vault's `vaultManager` is a decentralized party, but that fact is irrelevant to issuance. The decentralization is used only for vault-lifecycle governance (deploy, pause, limits). Individual mints and burns do not involve the committee — vaultManager's authority is captured once at vault-deploy time as a signatory on the `Vault` contract and inherited by every `DepositRequest` / `WithdrawRequest`.
