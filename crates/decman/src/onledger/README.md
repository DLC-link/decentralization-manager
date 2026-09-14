# `onledger`: Canton-native coordination

This module is the on-ledger foundation of the Noise sunset
(`docs/NOISE_SUNSET_DESIGN.md`, section 4). Part 1 gives one node identity,
one Daml codec, one Ledger API client, the registry, and the proposal
lifecycle. Part 2 adds the topology co-sign primitives, the member
validation rules, the run engine and its per-kind driver contract, the
observer loop, and the signatures of the contracts, DAR, and ACS pieces.
The per-kind step bodies (`engine/{onboarding,...}.rs`) and the bodies of
`submission.rs`, `dars.rs`, `acs.rs` are stubs for the kind agents.

Every timestamp in this module is an `i64` of **microseconds since the epoch**
(the Ledger API `Timestamp` unit) unless a name or comment says seconds.

## Files

| File | Responsibility |
|---|---|
| `mod.rs` | `OnLedger` facade held by `AppState`; `now_micros()`, `now_secs()`. |
| `identity.rs` | `NodeIdentity` from the `kind = 'node'` credentials row; `verify_hosting`. |
| `daml/templates.rs` | Template ids, module paths, choice names, `CoordinationPackage` resolver. |
| `daml/codec.rs` | Rust records for every template and choice argument; `Record` encode/decode. |
| `daml/client.rs` | `CoordinationClient`: create, exercise, and ACS reads as the node party. |
| `registry.rs` | `DecmanNode` publish/update/heartbeat, peer entries, `PeerHealthSnapshot`, version gate. |
| `proposals.rs` | `WorkflowProposal` create/accept/decline/cancel/finish, D6 counting, `pending_invitations` projection, archive sweep. |
| `topology.rs` | Discovery queries, pending/accepted reads, `propose_mapping`, `cosign_by_hash`, waits, root delegations, canonical mapping builders, unsolicited scan. |
| `validation.rs` | `Expectations` and the pure per-kind checks a member runs before it co-signs (design section 5). |
| `keys.rs` | The dual-usage party key and its root delegation, the local identity for validation, and the `dec_party_participant` key caches (design D4). |
| `engine/mod.rs` | `StartRequest`, `start_run`, `accept_invitation`, `decline_invitation`, `cancel_run`, `retry_run`, `RunMeta`, `TickCtx`, `KindDriver`, row helpers, quorum arithmetic. |
| `engine/{onboarding,add_party,kick,change_threshold,contracts,dars}.rs` | One `KindDriver` per kind with the section-6 step lists. Tick bodies are stubs. |
| `observer.rs` | `spawn_observer`: the polling loop, per-run `try_lock`, metrics. |
| `submission.rs` | Design D7 types and signatures (contracts rounds). Pure helpers and ACS reads implemented; ledger writes are stubs. |
| `dars.rs` | Design D8 types and signatures (DAR pins, embedded coordination DAR). Pure helpers implemented; participant calls are stubs. |
| `acs.rs` | Design D9 types and signatures (spool, manifests, import). Path helpers and the manifest read implemented; the rest are stubs. |

## Storage

Migration `000020_node_identity`:

```sql
ALTER TABLE party_credentials ADD COLUMN kind TEXT NOT NULL DEFAULT 'decparty';
CREATE TABLE proposal_decisions (
    proposal_cid TEXT PRIMARY KEY NOT NULL,
    decision TEXT NOT NULL,           -- 'accepted' | 'declined' | 'dismissed'
    decided_at INTEGER NOT NULL,      -- unix seconds
    pinned_hashes_json TEXT           -- JSON array of Canton hex hashes, or NULL
);
```

Rust side (`config.rs`, `db/rows.rs`, `db/schema.rs`):

```rust
pub enum CredentialKind { Decparty, Node }           // config.rs; serde lowercase; Default = Decparty
pub struct PartyCredentials { pub kind: CredentialKind, /* unchanged fields */ }

pub enum ProposalDecision { Accepted, Declined, Dismissed }     // db/rows.rs
pub struct ProposalDecisionEntry {
    pub proposal_cid: String, pub decision: ProposalDecision,
    pub decided_at: i64, pub pinned_hashes: Vec<String>,
}

// SchemaRead (SqlitePool)
async fn get_proposal_decision(&self, proposal_cid: &str) -> Result<Option<ProposalDecisionEntry>>;
async fn get_all_proposal_decisions(&self) -> Result<Vec<ProposalDecisionEntry>>;

// Commitable (sqlx::Transaction)
async fn delete_party_credentials(&mut self, dec_party_id: &CantonId) -> Result;
async fn insert_proposal_decision(&mut self, entry: &ProposalDecisionEntry) -> Result<bool>; // false = row existed, unchanged
async fn update_proposal_decision(&mut self, proposal_cid: &str, decision: ProposalDecision, decided_at: i64) -> Result;
async fn set_proposal_pinned_hashes(&mut self, proposal_cid: &str, pinned_hashes: &[String]) -> Result;
async fn delete_proposal_decision(&mut self, proposal_cid: &str) -> Result;
```

`insert_proposal_decision` is `INSERT OR IGNORE`: the first decision wins, so
a racing accept and decline cannot both take effect.

## Configuration (`consts.rs`)

```rust
pub const COORDINATION_PACKAGE_REF: &str = "#decman-coordination-v1";
pub fn coordination_package_ref() -> String;      // DECPM_COORDINATION_PACKAGE_REF
pub const COORDINATION_VERSION: i64 = 1;
pub fn heartbeat_interval_secs() -> u64;          // DECPM_HEARTBEAT_INTERVAL_SECS, default 3600, min 1
pub fn heartbeat_min_interval_secs() -> u64;      // DECPM_HEARTBEAT_MIN_INTERVAL_SECS, default 60, clamped to [1, interval]
pub fn peer_stale_factor() -> u64;                // DECPM_PEER_STALE_FACTOR, default 3, min 1
pub fn observer_poll_secs() -> u64;               // DECPM_OBSERVER_POLL_SECS, default 3, min 1
pub fn proposal_ttl_secs() -> u64;                // DECPM_PROPOSAL_TTL_SECS, default 604800, min 1
pub const UNSOLICITED_SCAN_INTERVAL_SECS: u64 = 60;   // unsolicited scan + archive sweep cadence
pub fn auto_upload_coordination_dar() -> bool;    // DECPM_AUTO_UPLOAD_COORDINATION_DAR, default true
pub fn acs_spool_dir(data_dir: &Path) -> PathBuf; // DECPM_ACS_SPOOL_DIR, default {data_dir}/acs
```

## `mod.rs`

```rust
pub fn now_micros() -> i64;
pub fn now_secs() -> i64;

pub struct OnLedger { /* private */ }
impl OnLedger {
    pub async fn new(config: NodeConfig, db: SqlitePool,
                     auth: Arc<RwLock<Option<WorkflowAuth>>>,
                     party_credentials: Arc<RwLock<Vec<PartyCredentials>>>,
                     test_mode: bool) -> Arc<Self>;          // loads the identity once; logs on failure
    pub fn detached(/* same args */) -> Arc<Self>;           // no identity load
    pub fn config(&self) -> &NodeConfig;
    pub fn db(&self) -> &SqlitePool;
    pub fn package(&self) -> &CoordinationPackage;
    pub fn test_mode(&self) -> bool;
    pub async fn reload_identity(&self) -> Result<Option<NodeIdentity>>;   // call after reload_auth
    pub async fn identity(&self) -> Option<NodeIdentity>;
    pub async fn require_identity(&self) -> Result<NodeIdentity>;
    pub async fn client(&self) -> Result<CoordinationClient>;
    pub fn registry(&self) -> Arc<RwLock<PeerHealthSnapshot>>;
    pub async fn registry_snapshot(&self) -> PeerHealthSnapshot;
    pub async fn vetted_peers(&self) -> Result<HashSet<CantonId>>;
    pub async fn refresh_registry(&self) -> Result<PeerHealthSnapshot>;     // observer tick
    pub async fn publish_registry_entry(&self) -> Result<PublishOutcome>;   // observer tick / startup task
    pub async fn heartbeat_if_due(&self) -> Result<Option<String>>;         // observer tick
    pub async fn read_registry(&self) -> Result<RegistryResponse>;          // GET /registry

    // Part 2 caches, replaced by the observer loop
    pub async fn pending_invitations(&self) -> Vec<PendingInvitation>;      // GET /invitations
    pub async fn set_pending_invitations(&self, list: Vec<PendingInvitation>);
    pub async fn remove_pending_invitation(&self, proposal_cid: &str);
    pub async fn unsolicited_proposals(&self) -> Vec<UnsolicitedProposal>;  // GET /proposals/unsolicited
    pub async fn set_unsolicited(&self, list: Vec<UnsolicitedProposal>);
    pub async fn run_lock(&self, instance_name: &str) -> Arc<tokio::sync::Mutex<()>>;  // try_lock per run
    pub async fn prune_run_locks(&self, live: &HashSet<String>);
}

// Re-exports at the module root
pub use engine::{AcceptedInvitation, KindDriver, MemberVariant, PreflightRejected, RunMeta, StartRequest,
                 StartedRun, TickCtx, accept_invitation, cancel_run, decline_invitation, retry_run, start_run};
pub use observer::spawn_observer;
pub use topology::UnsolicitedProposal;
```

`AppState.onledger: Arc<OnLedger>` is constructed in `start_server` after
the auth registry, with the same `auth` and `party_credentials` `Arc`s, so a
credentials change followed by `reload_identity()` is visible at once. Tests
use `OnLedger::placeholder()` (`#[cfg(test)]`).

`start_server` spawns the observer right after the reward automation task:
`crate::onledger::spawn_observer(app_state.onledger.clone())`. The loop
idles until a node identity exists.

## `identity.rs`

```rust
pub enum NodeTokenSource { Keycloak(Arc<TokenManager>), Mock(Arc<MockTokenManager>) }
impl NodeTokenSource { pub async fn token(&self) -> Result<String>; }

#[derive(Clone)]
pub struct NodeIdentity { pub node_party: CantonId, pub participant_id: CantonId, pub user_id: String, /* tokens */ }
impl NodeIdentity {
    pub fn new(node_party: CantonId, participant_id: CantonId, user_id: String, tokens: NodeTokenSource) -> Self;
    pub async fn token(&self) -> Result<String>;
    pub fn tokens(&self) -> &NodeTokenSource;
}

pub fn node_credentials(rows: &[PartyCredentials]) -> Option<&PartyCredentials>;   // the kind == Node row
pub async fn load_node_identity(config: &NodeConfig, rows: &[PartyCredentials],
                                auth: Option<&WorkflowAuth>) -> Result<Option<NodeIdentity>>;
pub fn require_node_identity(identity: Option<&NodeIdentity>) -> Result<&NodeIdentity>;

pub struct HostingCheck {
    pub mapping_exists: bool, pub hosted: bool, pub permission: Option<Permission>,
    pub onboarding: bool, pub threshold: u32,
}
impl HostingCheck { pub fn has_submission(&self) -> bool; pub fn describe(&self) -> String; }
pub fn hosting_check_from_mapping(mapping: Option<&PartyToParticipant>, participant: &CantonId) -> HostingCheck;  // pure
pub async fn verify_hosting(config: &NodeConfig, party: &CantonId, participant: &CantonId) -> Result<HostingCheck>;
```

`verify_hosting` reads the synchronizer head state over the tokenless Admin
API through `workflow::external_party::add_hosts::read_party_to_participant`,
which already requires `mapping.party == party`. `has_submission()` is the
one trust predicate of design D2: use it before naming a party as observer,
before counting an acceptance, and before accepting a node identity.

## `daml/templates.rs`

```rust
pub const PACKAGE_KEY: &str = "decman_coordination";
pub struct CoordinationPackage(/* ref */);
impl CoordinationPackage {
    pub fn from_env() -> Self;                 // consts::coordination_package_ref()
    pub fn new(package_ref: impl Into<String>) -> Self;
    pub fn reference(&self) -> &str;           // "#decman-coordination-v1"
    pub fn package_name(&self) -> &str;        // "decman-coordination-v1"
}
impl PackageResolver for CoordinationPackage;  // answers only PACKAGE_KEY

pub enum CoordinationTemplate { DecmanNode, WorkflowProposal, WorkflowAcceptance, WorkflowDecline,
                                WorkflowOutcome, SubmissionRound, SubmissionSignature, AcsManifest }
impl CoordinationTemplate {
    pub fn module(self) -> &'static str;
    pub fn entity(self) -> &'static str;
    pub fn identifier(self, package: &CoordinationPackage) -> Identifier;
    pub fn matches(self, id: &Identifier) -> bool;     // module + entity only
}
impl TemplateInfo for CoordinationTemplate;

pub mod modules { TYPES, NODE, WORKFLOW, SUBMISSION, ACS }   // "Decman.Coordination.*"
pub mod choices { DECMAN_NODE_HEARTBEAT, DECMAN_NODE_UPDATE, DECMAN_NODE_RETIRE,
                  WORKFLOW_PROPOSAL_ACCEPT, WORKFLOW_PROPOSAL_DECLINE, WORKFLOW_PROPOSAL_CANCEL,
                  WORKFLOW_PROPOSAL_FINISH, WORKFLOW_ACCEPTANCE_ARCHIVE, WORKFLOW_DECLINE_ARCHIVE,
                  WORKFLOW_OUTCOME_ARCHIVE, SUBMISSION_ROUND_SIGN, SUBMISSION_ROUND_CLOSE,
                  SUBMISSION_SIGNATURE_ARCHIVE, ACS_MANIFEST_ARCHIVE }
```

## `daml/codec.rs`

```rust
pub use common::types::WorkflowKind;   // Onboarding | Kick | Contracts | Dars | AddParty | ChangeThreshold

pub trait TemplateRecord: Sized {
    const TEMPLATE: CoordinationTemplate;
    fn to_record(&self) -> Record;
    fn from_record(rec: &Record) -> anyhow::Result<Self>;
    fn to_value(&self) -> Value;
}
pub trait ChoiceArgument { fn to_record(&self) -> Record; fn to_value(&self) -> Value; }
pub fn unit_argument() -> Value;   // for choices without arguments

pub struct DarPin { filename, sha256_hex, main_package_id, size_bytes: i64 }

// Template records (fields in Daml order; Option<_> for Optional; i64 micros for Time)
pub struct DecmanNodeRecord { node: CantonId, participant_id: String, display_name, version, build_version,
    coordination_version: i64, peers: Vec<CantonId>, last_active_at: i64, heartbeat_interval_secs: i64,
    min_heartbeat_interval_secs: i64 }
pub struct WorkflowProposalRecord { proposer, proposer_participant, proposer_namespace_fingerprint: Option<String>,
    proposer_signing_public_key_hex: Option<String>, proposer_daml_key_fingerprint: Option<String>, run_id,
    kind: WorkflowKind, invitees: Vec<CantonId>, participants: Vec<String>, dec_party_id: Option<String>,
    prefix: Option<String>, threshold: Option<i64>, previous_threshold: Option<i64>, dnd_base_serial: Option<i64>,
    p2p_base_serial: Option<i64>, new_participant: Option<String>, kicked_participant: Option<String>,
    dar_pins: Vec<DarPin>, package_names: Vec<String>, description, created_at: i64, expires_at: i64 }
pub struct WorkflowAcceptanceRecord { proposal: String /* cid */, proposer, acceptor, observers, run_id,
    participant_id, namespace_fingerprint, signing_public_key_hex, daml_key_fingerprint,
    member_party: Option<CantonId>, accepted_at }
pub struct WorkflowDeclineRecord { proposal, proposer, decliner, observers, run_id, reason, declined_at }
pub struct WorkflowOutcomeRecord { proposer, run_id, kind, observers, succeeded: bool, error: Option<String>, finished_at }
pub struct SubmissionRoundRecord { proposer, run_id, index: i64, signers, dec_party_id, act_as, description,
    prepared_transaction_hex, prepared_hash_hex, hashing_scheme_version: i64, preparation_time, max_record_time, deadline }
pub struct SubmissionSignatureRecord { round: String /* cid */, proposer, signer, observers, run_id, index,
    participant_id, signed_by, signature_hex, format, algorithm, signed_at }
pub struct AcsManifestRecord { exporter, exporter_participant, observers, dec_party_id, target_participant,
    activation_serial: i64, size_bytes: i64, sha256_hex, package_ids: Vec<String>, exported_at }

// Choice arguments
pub struct DecmanNodeUpdateArgs { new_peers, new_display_name, new_version, new_build_version,
    new_coordination_version, new_heartbeat_interval_secs, new_min_heartbeat_interval_secs }
impl DecmanNodeUpdateArgs { pub fn from_desired(desired: &DecmanNodeRecord) -> Self; }
pub struct AcceptArgs { acceptor: CantonId, participant_id, namespace_fingerprint, signing_public_key_hex,
    daml_key_fingerprint, member_party: Option<CantonId> }
pub struct DeclineArgs { decliner: CantonId, reason }
pub struct FinishArgs { succeeded: bool, error: Option<String> }
pub struct SignArgs { signer: CantonId, participant_id, signed_by, signature_hex, format, algorithm }
pub struct CloseArgs { result }
```

Decoders are strict: a missing field, a wrong shape, or an unknown enum
constructor is an error. Every template record is unit-tested for an
encode-decode round trip with all `Optional` fields `None` and `Some`.

## `daml/client.rs`

```rust
pub struct ExerciseOutcome { pub created_contract_id: Option<String>, pub update_id: String }
pub struct ActiveContract<T> { pub contract_id: String, pub offset: i64, pub record: T }

#[derive(Clone)]
pub struct CoordinationClient { /* config, identity, package, test_mode */ }
impl CoordinationClient {
    pub fn new(config: NodeConfig, identity: NodeIdentity, package: CoordinationPackage, test_mode: bool) -> Self;
    pub fn node_party(&self) -> &CantonId;
    pub fn participant_id(&self) -> &CantonId;
    pub fn identity(&self) -> &NodeIdentity;
    pub fn package(&self) -> &CoordinationPackage;
    pub fn config(&self) -> &NodeConfig;
    pub fn test_mode(&self) -> bool;
    pub async fn create<T: TemplateRecord>(&self, record: &T) -> Result<String>;                     // -> contract id
    pub async fn create_record(&self, template: CoordinationTemplate, arguments: Record) -> Result<String>;
    pub async fn exercise(&self, template: CoordinationTemplate, contract_id: &str, choice: &str,
                          argument: Value) -> Result<ExerciseOutcome>;
    pub async fn for_each_active<F>(&self, template: CoordinationTemplate, visit: F) -> Result<()>
        where F: FnMut(&str, i64, &Record) -> Result<()>;
    pub async fn list_active<T: TemplateRecord>(&self) -> Result<Vec<ActiveContract<T>>>;     // skips undecodable rows with a warning
}
```

Writes use `commands_envelope(uuid, node_party, node_party, cmds, [])` and
`CommandService.submit_and_wait_for_transaction`; `created_contract_id` is
the first created event, so a choice that returns a `ContractId` reports the
new contract and a choice that returns `()` reports `None`. Reads use
`server::reward_automation::for_each_active_created` as the node party
(wildcard filter plus in-memory match in test mode).

## `registry.rs`

```rust
pub fn desired_node_record(config: &NodeConfig, identity: &NodeIdentity, peers: &[Peer],
                           vetted_peers: &HashSet<CantonId>, last_active_at: i64) -> DecmanNodeRecord;
pub fn needs_update(current: &DecmanNodeRecord, desired: &DecmanNodeRecord) -> bool;   // ignores lastActiveAt
pub fn heartbeat_due(last_active_at: i64, heartbeat_interval_secs: i64, now: i64) -> bool;
pub fn is_stale(last_active_at: i64, heartbeat_interval_secs: i64, now: i64, factor: u64) -> bool;

pub async fn coordination_package_ids(config: &NodeConfig, package_name: &str) -> Result<HashSet<String>>;
pub async fn fetch_vetted_packages_for(config: &NodeConfig, participant_id: &CantonId) -> Result<HashSet<String>>;
pub async fn participant_has_vetted(config: &NodeConfig, participant_id: &CantonId,
                                    coordination_ids: &HashSet<String>) -> Result<bool>;
pub async fn vetted_peers(config: &NodeConfig, package_name: &str, self_participant: &CantonId,
                          peers: &[Peer]) -> Result<HashSet<CantonId>>;

pub enum PublishOutcome { Created(String), Updated(String), Unchanged(String) }
impl PublishOutcome { pub fn contract_id(&self) -> &str; }
pub async fn read_own_entry(client: &CoordinationClient) -> Result<Option<ActiveContract<DecmanNodeRecord>>>;
pub async fn publish_or_update(client: &CoordinationClient, desired: &DecmanNodeRecord) -> Result<PublishOutcome>;
pub async fn heartbeat_if_due(client: &CoordinationClient, current: &ActiveContract<DecmanNodeRecord>,
                              now: i64) -> Result<Option<String>>;
pub async fn retire(client: &CoordinationClient) -> Result<Option<String>>;

pub struct PeerEntry { pub contract_id: String, pub offset: i64, pub record: DecmanNodeRecord,
                       pub hosting: Option<HostingCheck> }
impl PeerEntry { pub fn hosting_verified(&self) -> bool; pub fn participant(&self) -> Option<CantonId>; }
pub async fn read_peer_entries(client: &CoordinationClient, config: &NodeConfig) -> Result<Vec<PeerEntry>>;

pub struct PeerHealth { pub node_party: Option<CantonId>, pub version: Option<String>, pub build_version: Option<String>,
    pub coordination_version: Option<i64>, pub last_active_at: Option<i64>, pub heartbeat_interval_secs: Option<i64>,
    pub vetted: bool, pub status: PeerHealthStatus }
#[derive(Default)]
pub struct PeerHealthSnapshot { pub entries: HashMap<CantonId /* participant */, PeerHealth>, pub refreshed_at: Option<i64> }
impl PeerHealthSnapshot { pub fn get(&self, participant: &CantonId) -> Option<&PeerHealth>; }
pub fn build_snapshot(peers: &[Peer], self_participant: &CantonId, entries: &[PeerEntry], vetted: &HashSet<CantonId>,
                      now: i64, stale_factor: u64, refreshed_at: i64) -> PeerHealthSnapshot;   // pure
pub fn preflight_unready_peers(snapshot: &PeerHealthSnapshot, invitees: &[CantonId]) -> Vec<(CantonId, String)>;  // pure

pub fn view_of(entry: &PeerEntry, now: i64, stale_factor: u64) -> DecmanNodeView;
pub fn registry_response(own: Option<&ActiveContract<DecmanNodeRecord>>, entries: &[PeerEntry], peers: &[Peer],
                         now: i64, stale_factor: u64) -> RegistryResponse;
```

Keying rule: an entry counts for a configured peer only when its signatory
(`record.node`) equals `peers.party`, its `participantId` claim equals
`peers.participant_id`, and `verify_hosting` reported Submission. Statuses:
`Active`, `Stale` (age > factor x interval), `Unknown` (vetted, no matching
entry), `Unvetted` (participant has not vetted the coordination package).
`preflight_unready_peers` is the design D3 version gate; the 409 text of a
start handler should join its reasons.

## `proposals.rs`

```rust
pub type ActiveProposal = ActiveContract<WorkflowProposalRecord>;
pub type Acceptance     = ActiveContract<WorkflowAcceptanceRecord>;
pub type Decline        = ActiveContract<WorkflowDeclineRecord>;
pub type Outcome        = ActiveContract<WorkflowOutcomeRecord>;

pub fn proposal_lifetime(now: i64) -> (i64, i64);    // (createdAt, expiresAt = now + DECPM_PROPOSAL_TTL_SECS)

pub async fn create_proposal(client: &CoordinationClient, record: &WorkflowProposalRecord) -> Result<String>;
pub async fn cancel(client: &CoordinationClient, proposal_cid: &str) -> Result<()>;
pub async fn finish(client: &CoordinationClient, proposal_cid: &str, succeeded: bool, error: Option<String>) -> Result<String>;
pub async fn accept(client: &CoordinationClient, proposal_cid: &str, args: &AcceptArgs) -> Result<String>;
pub async fn decline(client: &CoordinationClient, proposal_cid: &str, reason: &str) -> Result<String>;

pub async fn read_active_proposals_for_me(client: &CoordinationClient) -> Result<Vec<ActiveProposal>>;  // invitee, not proposer
pub async fn read_my_proposals(client: &CoordinationClient) -> Result<Vec<ActiveProposal>>;             // proposer == node party
pub async fn read_proposal(client: &CoordinationClient, proposal_cid: &str) -> Result<Option<ActiveProposal>>;  // D5 step 5 re-read
pub async fn read_acceptances(client: &CoordinationClient, proposal_cid: &str) -> Result<Vec<Acceptance>>;    // raw
pub async fn read_declines(client: &CoordinationClient, proposal_cid: &str) -> Result<Vec<Decline>>;
pub async fn read_outcomes(client: &CoordinationClient) -> Result<Vec<Outcome>>;

pub fn counted_acceptances(proposal: &ActiveProposal, acceptances: &[Acceptance],
                           hosted: &dyn Fn(&CantonId, &CantonId) -> bool) -> Result<Vec<Acceptance>>;   // pure, D6
pub async fn counted_acceptances_verified(config: &NodeConfig, proposal: &ActiveProposal,
                                          acceptances: &[Acceptance]) -> Result<Vec<Acceptance>>;

pub fn invitation_from_proposal(proposal: &ActiveProposal, peers: &[Peer], received_at: i64) -> PendingInvitation;
pub fn plan_projection(proposals: &[ActiveProposal], decisions: &[ProposalDecisionEntry], existing: &[PendingInvitation],
                       peers: &[Peer], now_micros: i64) -> (Vec<PendingInvitation>, Vec<String>);  // (upserts, deletes)
pub async fn project_pending_invitations(db: &SqlitePool, proposals: &[ActiveProposal], decisions: &[ProposalDecisionEntry],
                                         peers: &[Peer], now_micros: i64) -> Result<Vec<PendingInvitation>>;

pub struct ArchiveSweepReport { pub acceptances: usize, pub declines: usize }
pub async fn archive_sweep(client: &CoordinationClient, active_proposal_cids: &HashSet<String>) -> Result<ArchiveSweepReport>;
```

`counted_acceptances` keeps an acceptance only when `proposal == cid`,
`proposer` matches, the acceptor is an invitee and not the proposer,
`participantId` is in `participants`, and `hosted(acceptor, participant)`
holds. A second counted acceptance from one acceptor is an error (fail
closed). `project_pending_invitations` returns the whole table afterwards;
the caller replaces `AppState.pending_invitations` with it.

Transitional rule: until migration `000021` renames the column, an on-ledger
`pending_invitations` row stores the proposer's participant id in
`coordinator_pubkey`, and `id` is the proposal contract id. Rows whose
`coordinator_pubkey` is not a Canton id are Noise rows and are left alone.

## HTTP surface (`server/handlers/node_identity.rs`)

| Route | Auth | Body / response |
|---|---|---|
| `GET /node-identity` | admin | `NodeIdentityResponse { configured, node_party_id?, participant_id, user_id?, hosting_permission? }` |
| `PUT /node-identity` | admin; unauthenticated only while `party_credentials` is empty | `NodeIdentityRequest` -> `NodeIdentityResponse`; 400 unless this participant hosts the party with Submission |
| `GET /registry` | admin | `RegistryResponse { self_entry?, peers, inbound }`; 409 without a node identity |

DTOs live in `crates/common/src/coordination.rs` and are exported by
`gen-types`: `NodeIdentityRequest`, `NodeIdentityResponse`, `PeerHealthStatus`
(`Active | Stale | Unknown | Unvetted`), `DecmanNodeView`, `RegistryResponse`.

Side effects of the PUT: the row is written with `kind = 'node'` and the node
party in both `dec_party_id` and `member_party_id`; any older node row is
deleted; `reload_auth` rebuilds the registry; `OnLedger::reload_identity`
picks up the token manager. `GET /auth/status` and the inbound JWT trust set
(`find_trusted`) skip `kind = 'node'` rows.

## `topology.rs`

Every read targets the physical synchronizer store with
`time_query = Snapshot(9999-12-31T23:59:59.999999Z)` and
`operation = ADD_REPLACE`. `HeadState` hides a fresh proposal until the node
observes a sequencer timestamp past `valid_from`; "effective" is therefore
`valid_from <= now` on the local clock.

```rust
pub const HASH_PURPOSE_TOPOLOGY_TRANSACTION_SIGNATURE: i32 = 11;
pub const HASH_PURPOSE_DECENTRALIZED_NAMESPACE: i32 = 37;

pub fn max_timestamp() -> Timestamp;                       // Daml-LF max
pub fn proposals_query(sync_id: &str) -> BaseQuery;        // proposals = true
pub fn accepted_query(sync_id: &str) -> BaseQuery;         // proposals = false
pub fn timestamp_micros(ts: &Timestamp) -> i64;
pub fn is_effective(valid_from: Option<&Timestamp>, now: i64) -> bool;

pub struct PendingProposal<M> { hash_hex: String, serial: u32, signed_by: Vec<String>, operation: i32,
                                mapping: M, sequenced: Option<Timestamp>, valid_from: Option<Timestamp> }
impl<M> PendingProposal<M> { pub fn is_add_replace(&self) -> bool; pub fn is_signed_by(&self, fp: &str) -> bool; }
pub struct AcceptedMapping<M> { serial: u32, mapping: M, valid_from: Option<Timestamp> }
pub fn hash_hex_of(context: &BaseResult) -> String;        // lowercase hex of transaction_hash

pub async fn list_pending_dnd(config, sync_id, namespace: &str) -> Result<Vec<PendingProposal<DecentralizedNamespaceDefinition>>>;
pub async fn list_pending_p2p(config, sync_id, party: &CantonId) -> Result<Vec<PendingProposal<PartyToParticipant>>>;
pub async fn read_accepted_dnd(config, sync_id, namespace: &str) -> Result<Option<AcceptedMapping<DecentralizedNamespaceDefinition>>>;
pub async fn read_accepted_p2p(config, sync_id, party: &CantonId) -> Result<Option<AcceptedMapping<PartyToParticipant>>>;

pub fn transaction_hash_of(versioned_transaction: &[u8]) -> String;   // multihash sha256(be32(11) || bytes)
pub fn compute_namespace<'a>(owners: impl IntoIterator<Item = &'a String>) -> String;  // sorted, deduped

pub struct ProposedTx { hash_hex: String, transaction: SignedTopologyTransaction }
pub async fn propose_mapping(config, sync_id, mapping: TopologyMapping, serial: u32) -> Result<ProposedTx>;
pub enum CosignOutcome { NotFound, Signed, AlreadySigned }
pub async fn cosign_by_hash(config, sync_id, hash_hex: &str, previously_signed_by: &[String]) -> Result<CosignOutcome>;

pub struct WaitBudget { max_attempts: usize, delay: Duration }   // Default = DECPM_TOPOLOGY_RETRY_*
pub enum MappingKey { Dnd(String), P2p(CantonId) }
pub struct AcceptedState { serial: u32, valid_from: Option<Timestamp> }
pub async fn read_accepted_state(config, sync_id, key: &MappingKey) -> Result<Option<AcceptedState>>;
pub async fn wait_effective(config, sync_id, key: &MappingKey, serial: u32, budget: WaitBudget) -> Result<AcceptedState>;

pub async fn read_root_delegation(config, sync_id, fp: &str) -> Result<Option<(SigningPublicKey, Option<Timestamp>)>>;
pub async fn read_root_delegation_key(config, sync_id, fp: &str) -> Result<SigningPublicKey>;   // asserts fingerprint
pub async fn wait_owner_root_delegations(config, sync_id, fps: &[String], budget: WaitBudget) -> Result<BTreeMap<String, SigningPublicKey>>;

pub struct UnsolicitedProposal { mapping: String, key: String, hash_hex: String, serial: u32, signed_by: Vec<String>, sequenced_at: Option<i64> }
pub async fn scan_unsolicited(config, sync_id) -> Result<Vec<UnsolicitedProposal>>;

// Builders: owners sorted, hosts sorted by uid, keys sorted by fingerprint, one per key
pub fn dnd_of(m: &TopologyMapping) -> Option<&DecentralizedNamespaceDefinition>;
pub fn p2p_of(m: &TopologyMapping) -> Option<&PartyToParticipant>;
pub fn build_dnd(owners: &[String], threshold: u32) -> TopologyMapping;
pub fn build_bootstrap_p2p(prefix: &str, namespace: &str, hosts: &[CantonId], keys: &[SigningPublicKey], threshold: u32) -> TopologyMapping;
pub fn build_add_party_dnd(head: &DecentralizedNamespaceDefinition, joiner_fp: &str, threshold: u32) -> TopologyMapping;
pub fn build_add_party_p2p(head: &PartyToParticipant, joiner: &CantonId, joiner_key: &SigningPublicKey, threshold: u32) -> TopologyMapping;
pub fn build_kick_dnd(head: &DecentralizedNamespaceDefinition, kicked_fp: &str, threshold: u32) -> TopologyMapping;
pub fn build_kick_p2p(head: &PartyToParticipant, kicked: &CantonId, kicked_key_fp: &str, threshold: u32) -> TopologyMapping;
pub fn build_change_threshold_dnd(head: &DecentralizedNamespaceDefinition, threshold: u32) -> TopologyMapping;
pub fn build_change_threshold_p2p(head: &PartyToParticipant, threshold: u32) -> TopologyMapping;
pub fn mapping_bytes(m: &TopologyMapping) -> Vec<u8>;
```

`propose_mapping` and `cosign_by_hash` go through
`workflow::topology::authorize_with_topology_retry` with
`must_fully_authorize = false`, `signed_by = []`, no force flags, and the
synchronizer store. `cosign_by_hash` maps `TOPOLOGY_TRANSACTION_NOT_FOUND`
to `NotFound` (retry next poll). `UnsolicitedProposal` belongs in
`common/src/coordination.rs` (TODO in the source); it lives here until that
file is open for edits. The repository holds no external vector for the
topology hash; the unit test pins the construction
`sha256(be32(11) || bytes)` against an independently computed value.

## `validation.rs`

```rust
pub struct ValidationError(pub String);   // Display + Error
pub type Check = Result<(), ValidationError>;

pub struct HostTuple { uid: String, permission: i32, onboarding: bool }
pub fn host_tuples(m: &PartyToParticipant) -> BTreeSet<HostTuple>;
pub fn key_bytes(m: &PartyToParticipant) -> BTreeSet<Vec<u8>>;          // serialized SigningPublicKey
pub fn key_fingerprints(m: &PartyToParticipant) -> BTreeSet<String>;

pub struct LocalIdentity { participant_id: CantonId, owner_fingerprints: BTreeSet<String>, daml_key_fingerprint: Option<String> }
pub struct KickedMember { participant_id: String, owner_fingerprint: String, signing_key_fingerprint: Option<String> }
pub struct CountedAcceptance { acceptor, participant_id, namespace_fingerprint, signing_public_key_hex, daml_key_fingerprint }
pub struct HeadState { dnd: Option<DecentralizedNamespaceDefinition>, p2p: Option<PartyToParticipant> }

pub struct Expectations { kind, proposer_participant, proposer_fingerprint: Option<String>, proposer_hosting_ok: bool,
    participants, prefix, dec_party_id, threshold: Option<u32>, previous_threshold, dnd_base_serial, p2p_base_serial,
    new_participant, kicked_participant, accepted: Vec<CountedAcceptance>, head: HeadState,
    owner_keys: BTreeMap<String, SigningPublicKey>, identity: LocalIdentity, kicked: Option<KickedMember>,
    survivor_key_claims: BTreeMap<String, String> }
impl Expectations {
    pub fn new(proposal: &WorkflowProposalRecord, counted: &[Acceptance], head: HeadState, identity: LocalIdentity) -> Self;
    pub fn with_owner_keys(self, keys) -> Self;  pub fn with_kicked(self, kicked) -> Self;
    pub fn with_survivor_key_claims(self, claims) -> Self;  pub fn with_proposer_hosting(self, ok: bool) -> Self;
    pub fn required_threshold(&self) -> Result<u32, ValidationError>;
    pub fn required_proposer_fingerprint(&self) -> Result<&str, ValidationError>;
    pub fn onboarding_owners(&self) -> Result<BTreeSet<String>, ValidationError>;
    pub fn onboarding_hosts(&self) -> BTreeSet<HostTuple>;
    pub fn joiner(&self) -> Result<&CountedAcceptance, ValidationError>;
}

pub struct SerialContext { accepted: Option<u32>, base: Option<u32> }
pub fn check_common<M>(pending: &PendingProposal<M>, exp: &Expectations, serials: &SerialContext) -> Check;
pub fn check_own_owner(dnd, exp) -> Check;
pub fn check_own_host(p2p, exp, as_joiner: bool) -> Check;
pub fn check_onboarding_dnd(pending, exp) -> Check;       pub fn check_onboarding_p2p(pending, exp) -> Check;
pub fn check_add_party_dnd(pending, exp) -> Check;        pub fn check_add_party_p2p(pending, exp) -> Check;
pub fn check_kick_dnd(pending, exp) -> Check;             pub fn check_kick_p2p(pending, exp) -> Check;   // legacy branch inside
pub fn check_change_threshold_dnd(pending, exp) -> Check; pub fn check_change_threshold_p2p(pending, exp) -> Check;
pub fn validate_dnd(pending, exp, accepted_serial: Option<u32>) -> Check;   // common + kind dispatch, DND base serial
pub fn validate_p2p(pending, exp, accepted_serial: Option<u32>) -> Check;   // common + kind dispatch, P2P base serial
```

Every check fails closed: a missing threshold, proposer fingerprint, head
mapping, cached kicked owner key, or root delegation key is an error. The
caller sets `proposer_hosting_ok` from `verify_hosting(proposer,
proposer_participant).has_submission()` and fills `owner_keys` from
`topology::wait_owner_root_delegations`. The kick P2P rule pins the removed
key to the kicked owner fingerprint on a dual-key party (signing key
fingerprints equal the DND owners) and uses elimination alone on a legacy
party: exactly one key removed, not this node's, not claimed by a survivor.

## `keys.rs`

Key material every kind driver shares (design D4). One vault key per member
per new party, named `{prefix}-key`, with usages `[Namespace, Protocol]`.
Its self-signed root `NamespaceDelegation` carries the key bytes into the
synchronizer store; the same key is the member's Daml signing key. Legacy
parties (`{prefix}-namespace` + `{prefix}-daml-transactions`) keep working:
every lookup is chain first, then the local caches, then the legacy vault
names.

```rust
pub fn party_key_name(prefix: &str) -> String;              // "{prefix}-key"
pub fn legacy_namespace_key_name(prefix: &str) -> String;   // "{prefix}-namespace"
pub use workflow::signing_keys::party_daml_key_name as legacy_daml_key_name;   // "{prefix}-daml-transactions"

pub struct PartyKey { pub key: SigningPublicKey, pub fingerprint: String, pub key_hex: String }
impl PartyKey { pub fn from_key(key: SigningPublicKey) -> Self; }   // fingerprint + lowercase hex of the prost bytes
pub fn decode_key_hex(key_hex: &str) -> Result<SigningPublicKey>;
pub fn proposer_key_material(key: &PartyKey) -> engine::ProposerKeyMaterial;   // ns fp, key hex, daml fp = ns fp

pub async fn ensure_party_key(config: &NodeConfig, prefix: &str) -> Result<PartyKey>;
pub async fn wait_own_root_delegation(config: &NodeConfig, sync_id: &str, fingerprint: &str, budget: WaitBudget) -> Result<()>;

pub struct VaultKey { pub name: String, pub key: SigningPublicKey, pub fingerprint: String }
impl VaultKey { pub fn has_usage(&self, usage: SigningKeyUsage) -> bool; }
pub async fn list_vault_keys(config: &NodeConfig) -> Result<Vec<VaultKey>>;   // ListMyKeys, no filter

pub async fn local_identity_for_party(config: &NodeConfig, db: &SqlitePool, dec_party_id: Option<&CantonId>,
                                      prefix: Option<&str>) -> Result<validation::LocalIdentity>;

pub async fn kicked_member(config: &NodeConfig, db: &SqlitePool, dec_party_id: &CantonId,
                           kicked_participant: &CantonId) -> Result<Option<validation::KickedMember>>;
pub fn kicked_member_from_rows(rows: &[DecPartyParticipantRow], kicked_uid: &str) -> Option<KickedMember>;   // pure
pub async fn survivor_key_claims(db: &SqlitePool, dec_party_id: &CantonId) -> Result<BTreeMap<String, String>>;  // uid -> daml fp
pub fn key_claims_from_rows(rows: &[DecPartyParticipantRow]) -> BTreeMap<String, String>;                     // pure
pub async fn record_member_keys(db: &SqlitePool, dec_party_id: &CantonId, participant: &CantonId,
                                owner_fp: Option<&str>, signing_fp: Option<&str>) -> Result<()>;
```

`ensure_party_key` is idempotent: it reuses the vault key by exact name
(and refuses a same-named key without both usages), then publishes the root
delegation to the Authorized store with `must_fully_authorize = true`, the
same request legacy onboarding sent. The publish is skipped when the
delegation is already in the synchronizer store, or already in the
Authorized store and waiting for dispatch, because Canton rejects a
duplicate mapping. A coordinator calls it in `prepare` before the proposal
exists (D6); a member calls it at its `GenerateKeys` step and then
`wait_own_root_delegation` before it accepts.

`local_identity_for_party`: `participant_id` from the config. With a party
id, `owner_fingerprints` = Namespace-usage vault keys that own the head DND,
plus the `{prefix}-key` when the vault holds it (an add-party joiner owns
nothing yet); `daml_key_fingerprint` = the one Protocol-usage vault key in
the head P2P `party_signing_keys`, else
`signing_keys::own_signing_key_fingerprint` (`dec_party_identity`, then the
legacy vault name). Without a party id (onboarding), `owner_fingerprints` =
every Namespace-usage vault key and `daml_key_fingerprint` = the
`{prefix}-key`, else the legacy `{prefix}-daml-transactions` key.

`kicked_member` reads `dec_party_participant.owner_key` / `signing_key` for
the kicked participant from this node's cache only (section 5). `Ok(None)`
means no cached owner key; an error means the cached key is not an owner of
the head DND (stale cache, or the kick already landed). `survivor_key_claims`
returns every cached claim, the kicked member's included; `check_kick_p2p`
skips it by uid. `record_member_keys` writes what a counted acceptance said
about its acceptor (design M6) through `update_participant_owner_key` /
`update_participant_signing_key`; a participant without a cached row is
skipped with a warning until the next parties refresh creates it.

## `engine/mod.rs`

```rust
pub const RUN_META_KEY: &str = "onledger";            // config_json key of RunMeta (until migration 000021)
pub const COMPLETE_STEP: &str = "Complete";
pub const WAITING_FOR_ACCEPTANCES_STEP: &str = "WaitingForAcceptances";

pub enum StartRequest {
    Onboarding { party_id_prefix, peer_ids: Vec<CantonId>, threshold: Option<i32>, instance_name },
    AddParty { dec_party_id, new_participant_id, new_threshold: i32, previous_threshold: i32, instance_name },
    Kick { dec_party_id, participant_id, new_threshold, previous_threshold, instance_name },
    ChangeThreshold { dec_party_id, new_threshold, previous_threshold, instance_name },
    Contracts { dec_party_id, participant_ids, participant_parties, operator_party, contracts: Vec<ContractDefinition>, instance_name },
    Dars { dar_files: Vec<DarFile>, peer_ids, instance_name },
}
impl StartRequest { pub fn kind(&self) -> WorkflowKind; pub fn instance_name(&self) -> &str;
                    pub fn dec_party_id(&self) -> Option<&CantonId>; pub fn threshold(&self) -> Option<i32>;
                    pub fn previous_threshold(&self) -> Option<i32>; }   // 0 = unknown -> None

pub struct PreflightRejected { message: String, peers: Vec<(CantonId, String)> }   // Error; downcast for 409
pub struct ProposerKeyMaterial { namespace_fingerprint, signing_public_key_hex, daml_key_fingerprint: Option<String> }
pub struct ProposalExtras { keys: ProposerKeyMaterial, dar_pins: Vec<DarPin>, package_names: Vec<String> }
pub struct StartedRun { instance_name: String, proposal_cid: String }

pub enum MemberVariant { Joiner, Member }                // serde PascalCase
pub struct RunMeta { proposal_cid, coordinator_party: CantonId, coordinator_participant: CantonId,
                     member_variant: Option<MemberVariant>, topology_hashes: BTreeMap<String, String> }
pub fn read_run_meta(run: &WorkflowRun) -> Option<RunMeta>;        // None = not an on-ledger row
pub async fn write_run_meta(db, instance_name, meta) -> Result<()>;
pub async fn pin_topology_hash(db, instance_name, key: &str /* dnd | p2p | clear */, hash_hex) -> Result<()>;

pub fn steps_for(kind, role, variant: Option<MemberVariant>) -> &'static [&'static str];
pub fn required_owner_signatures(previous_threshold: u32, new_threshold: u32) -> u32;   // max(prev, new, 1)
pub fn acceptances_needed(previous_threshold: u32, new_threshold: u32) -> u32;          // required - 1
pub fn needs_every_invitee(kind) -> bool;                                               // !(Kick | ChangeThreshold)

pub async fn advance_step(db, run: &WorkflowRun, step: &str) -> Result<()>;
pub async fn fail_run(db, run, error: &str) -> Result<()>;
pub async fn cancel_run_row(db, run, reason: &str) -> Result<()>;
pub async fn complete_run(db, run) -> Result<()>;

pub struct ProposalSnapshot { proposals, acceptances, declines, outcomes, decisions: Vec<ProposalDecisionEntry> }
impl ProposalSnapshot {
    pub async fn read(client, db) -> Result<Self>;
    pub fn proposal(&self, cid) -> Option<&ActiveProposal>;  pub fn acceptances_for(&self, cid) -> Vec<Acceptance>;
    pub fn declines_for(&self, cid) -> Vec<&Decline>;         pub fn outcome_for(&self, proposer, run_id) -> Option<&Outcome>;
    pub fn decision(&self, cid) -> Option<&ProposalDecisionEntry>;  pub fn for_me(&self, me) -> Vec<ActiveProposal>;
    pub fn active_cids(&self) -> HashSet<String>;
}
pub struct TickCtx<'a> { ol: &'a OnLedger, client: &'a CoordinationClient, identity: &'a NodeIdentity, sync_id: String,
                         participant_id: CantonId, proposals: &'a ProposalSnapshot, registry: &'a PeerHealthSnapshot, now_micros: i64 }
impl TickCtx<'_> { pub fn db(&self) -> &SqlitePool; pub fn is_expired(&self, p: &ActiveProposal) -> bool; }

#[allow(async_fn_in_trait)]
pub trait KindDriver {
    fn kind() -> WorkflowKind;
    fn coordinator_steps() -> &'static [&'static str];
    fn member_steps(variant: Option<MemberVariant>) -> &'static [&'static str];
    async fn preflight(ol: &OnLedger, req: &StartRequest) -> Result<()>;            // default Ok; return PreflightRejected for 409
    async fn prepare(ol: &OnLedger, req: &StartRequest) -> Result<ProposalExtras>;  // default empty; key material, pins, package names
    async fn tick_coordinator(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()>;
    async fn tick_member(ctx: &TickCtx<'_>, run: &WorkflowRun, meta: &RunMeta) -> Result<()>;
}
pub enum Driven { Skipped, Stopped, Ticked }
pub async fn drive(ctx: &TickCtx<'_>, run: &WorkflowRun) -> Result<Driven>;     // reconcile (D10) then dispatch

pub async fn start_run(ol: &OnLedger, req: StartRequest) -> Result<StartedRun>;
pub struct AcceptedInvitation { instance_name: String, member_variant: Option<MemberVariant> }
pub async fn accept_invitation(ol, proposal_cid) -> Result<AcceptedInvitation>;
pub async fn decline_invitation(ol, proposal_cid, reason) -> Result<String /* decline cid */>;
pub async fn cancel_run(ol, instance_name) -> Result<()>;
pub async fn retry_run(ol, instance_name) -> Result<()>;
pub fn peer_instance_name(kind, coordinator_participant: &str, run_id: &str) -> String;   // peer-{kind}-{16 chars}-{run_id}
pub fn member_variant_for(proposal: &WorkflowProposalRecord, me: &CantonId) -> Option<MemberVariant>;
pub fn accept_args(identity, keys: &ProposerKeyMaterial, member_party: Option<CantonId>) -> AcceptArgs;
```

`start_run` resolves the participants (head P2P hosts for party kinds,
`peer_ids` plus self otherwise), refuses with `PreflightRejected` when a
peer has no node party, is unready (`registry::preflight_unready_peers`),
or a threshold is out of range (section 6), runs the kind's `preflight` and
`prepare`, creates the `WorkflowProposal` with the base serials read from
the accepted DND/P2P, and only then writes the coordinator row at its first
step with `RunMeta`. `accept_invitation` writes `proposal_decisions`
(`INSERT OR IGNORE`, so a race with decline resolves to the first writer),
the peer row at its first step, and drops the card; the observer does the
rest. `drive` applies design D10 before the kind driver: a coordinator fails
its run when the proposal vanished, expired, or a decline breaks the quorum
(all invitees, or `acceptances_needed` for kick and change-threshold) and
exercises `Finish { succeeded = false }`; a peer completes or cancels its
row from the `WorkflowOutcome` when the proposal vanished.

Transitional rule: `RunMeta` lives in `config_json["onledger"]` and
`coordinator_pubkey` carries the coordinator's participant id. Migration
`000021` moves both to their own columns; the `TODO(migration 000021)`
comments mark every site.

## `engine/{kind}.rs`

Each file exports `pub struct <Kind>;` implementing `KindDriver`, plus the
step lists as `pub const COORDINATOR_STEPS` and `pub const MEMBER_STEPS`
(add-party also `JOINER_STEPS`). Every `prepare` that needs proposer key
material or DAR pins returns `bail!("not implemented: ...")`, so a start
fails closed until the kind agent lands; `Contracts::prepare` returns the
package names from `contracts[].package_id`. Every tick calls
`engine::not_implemented(run)` (debug log) and returns `Ok(())`.

## `observer.rs`

```rust
pub fn spawn_observer(ol: Arc<OnLedger>) -> JoinHandle<()>;   // registers metrics, spawns run_observer_loop
pub async fn run_observer_loop(ol: Arc<OnLedger>);            // interval(observer_poll_secs), MissedTickBehavior::Skip
pub struct ObserverState { /* last unsolicited scan */ }
pub struct TickReport { had_identity, runs_seen, runs_driven, runs_busy, runs_stopped, invitations: usize }
pub async fn tick(ol: &OnLedger, state: &mut ObserverState) -> TickReport;   // never panics
```

Tick order: identity (re-loaded when missing) -> `refresh_registry`,
`publish_registry_entry`, `heartbeat_if_due` -> in-progress runs ->
`ProposalSnapshot::read` -> `project_pending_invitations` into
`OnLedger::set_pending_invitations` -> synchronizer id -> `engine::drive`
for every row with `RunMeta`, concurrently, each under `run_lock().try_lock()`
(busy runs are skipped) -> every `UNSOLICITED_SCAN_INTERVAL_SECS`:
`topology::scan_unsolicited` into `set_unsolicited` and
`proposals::archive_sweep`. Runs are read before the snapshot so a row that
exists always has its proposal in the same tick.

Metrics: `decman_observer_tick_total`, `decman_observer_tick_seconds`
(histogram), `decman_observer_last_tick_seconds` (gauge),
`decman_observer_runs_driven_total`, `decman_observer_errors_total{stage}`
(`identity | registry | runs | proposals | projection | synchronizer | drive |
unsolicited | archive`), `decman_observer_no_identity_total`.

## `submission.rs` (design D7)

```rust
pub const MAX_RECORD_TIME_HORIZON_MICROS: i64;   // 20 h
pub const DEADLINE_SAFETY_MARGIN_MICROS: i64;    // 30 min
pub struct PreparedRound { index, description, prepared_transaction_hex, prepared_hash_hex, hashing_scheme_version, preparation_time, max_record_time, deadline }
pub struct VerifiedSignature { signed_by, signature: Vec<u8>, format, algorithm, participant_id }
pub fn deadline_for(preparation_time, max_record_time, tolerance_micros) -> i64;            // implemented
pub async fn read_record_time_tolerance(config, sync_id) -> Result<i64>;                     // stub
pub async fn prepare_rounds(ol, run, dec_party_id, contracts) -> Result<Vec<PreparedRound>>; // stub
pub async fn open_rounds(client, run_id, signers, dec_party_id, act_as, rounds) -> Result<Vec<String>>;  // stub
pub async fn read_rounds_for_run(client, run_id) -> Result<Vec<ActiveContract<SubmissionRoundRecord>>>;  // implemented
pub async fn read_signatures_for_round(client, round_cid) -> Result<Vec<ActiveContract<SubmissionSignatureRecord>>>;  // implemented
pub fn verify_signature(head_p2p, sig, hash) -> Result<VerifiedSignature>;                  // stub
pub fn dedupe_verified(sigs) -> BTreeMap<String, VerifiedSignature>;                        // implemented
pub async fn execute_round(ol, dec_party_id, round, signatures) -> Result<String>;          // stub
pub async fn close_round(client, round_cid, result) -> Result<()>;                          // implemented
pub fn check_round(round, accepted, head_p2p, own_key_fp, now_micros) -> Result<()>;        // stub (member rules)
pub async fn sign_round(ol, round, dec_party_id) -> Result<String>;                         // stub
pub async fn archive_own_signatures(client, active_round_cids) -> Result<usize>;            // implemented
```

## `dars.rs` (design D8)

```rust
pub const COORDINATION_DAR_FILENAME: &str;
pub fn embedded_coordination_dar() -> &'static [u8];              // include_bytes! of releases/v1/decman-coordination-v1-0.1.0.dar
pub fn hash_dar(bytes) -> String;                                 // lowercase hex sha256
pub fn decode_dar_files(files: &[DarFile]) -> Result<Vec<(String, Vec<u8>)>>;
pub fn pin_dar_files(files, main_package_ids: &BTreeMap<String, String>) -> Result<Vec<DarPin>>;
pub fn matching_pin<'a>(pins: &'a [DarPin], bytes) -> Option<&'a DarPin>;
pub async fn upload_and_vet_locally(config, filename, bytes, expected_main_package_id: Option<&str>) -> Result<String>;  // stub
pub async fn unvetted_pins_by_participant(config, participants, pins) -> Result<BTreeMap<CantonId, Vec<String>>>;      // stub
pub struct CoordinationDarState { uploaded, vetted, attempts, last_error }
pub async fn ensure_coordination_dar(config) -> Result<CoordinationDarState>;               // stub
```

## `acs.rs` (design D9)

```rust
pub struct SpoolFile { path, size_bytes, sha256_hex, package_ids }
pub fn spool_dir(config) -> PathBuf;                                                        // consts::acs_spool_dir
pub fn spool_path(config, party, target, activation_serial) -> PathBuf;                     // {prefix}-{namespace}-{target prefix}-{serial}.acs.gz
pub async fn capture_export_offset(db, config, run, party, joiner, base_serial) -> Result<i64>;   // stub
pub async fn export_snapshot(config, party, target, begin_offset_exclusive, path) -> Result<SpoolFile>;  // stub
pub async fn publish_manifest(client, observers, party, target, activation_serial, file) -> Result<String>;  // stub
pub async fn read_manifests(client, party) -> Result<Vec<ActiveContract<AcsManifestRecord>>>;   // implemented
pub fn verify_manifest(manifest, exporter_hosting, head_p2p, peers, joiner, activation_serial) -> Result<()>;  // stub (D9 rules 1-4)
pub async fn import_snapshot(config, db, party, manifest, path) -> Result<()>;              // stub
pub async fn clear_onboarding_flag(config, party, pre_activation_offset) -> Result<()>;     // stub
pub async fn cleanup_spool(config, party, target) -> Result<usize>;                         // stub
```

## Verification

```
DECMAN_SKIP_FRONTEND=1 cargo check -p decman --tests
DECMAN_SKIP_FRONTEND=1 cargo test -p decman --lib onledger   # onledger::keys for this file alone
DECMAN_SKIP_FRONTEND=1 cargo clippy -p decman --all-targets --all-features --no-deps -- -D warnings
cargo fmt -p decman -- --check
```
