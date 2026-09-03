//! Domain model and gRPC codecs for the decman on-chain governance protocol.
//!
//! `framework` is template-agnostic: traits, encode/record toolkits, event
//! filters, the command envelope. `catalog` is decman's protocol content:
//! the by-value `ActionType` enum, one struct per proposal, template
//! accessors, flow builders, and state interpretation.
//!
//! # Purity contract
//!
//! Every function in this crate is pure. The crate bans four things, and a
//! change that adds one breaks the contract:
//!
//! * **No I/O.** The crate opens no socket, file, or database connection.
//! * **No transport.** The crate builds and reads Ledger API values. It never
//!   submits them. The caller owns the gRPC channel.
//! * **No persistence.** The crate holds no state between calls.
//! * **No nondeterminism.** The crate reads no clock and draws no random
//!   number. The same inputs always give the same output.
//!
//! Everything the four bans exclude enters as a parameter instead: the caller
//! passes `now_micros`, the command id, the registry choice context, and the
//! resolved package refs. That is what makes an encode testable against a
//! fixed snapshot, and what lets an integrator drive the crate from any
//! transport.
//!
//! The `Cargo.toml` dependency list is the check a reader can run. It holds
//! `serde`, `thiserror`, `tracing`, `chrono`, the two canton crates, and this
//! workspace's `common`. A new dependency that does I/O, keeps state, or
//! reads a clock breaks the contract.
//!
//! # Quick start: a custom proposal
//!
//! An integrator adds a new proposal kind outside the crate by implementing
//! [`framework::TemplateInfo`], [`framework::DamlProtoEncode`], and
//! [`framework::Validate`] (default body accepts), then handing the payload
//! to [`framework::commands::build_propose`]:
//!
//! ```
//! use decman_lib::{CantonId, Error};
//! use decman_lib::canton_proto_rs::com::daml::ledger::api::v2::Value;
//! use decman_lib::catalog::commands::build_confirm_proposal;
//! use decman_lib::catalog::interpret::parse_domain_confirmation;
//! use decman_lib::framework::commands::build_propose;
//! use decman_lib::framework::encode::{field, make_record, make_text};
//! use decman_lib::framework::{
//!     DamlProtoEncode, PackageResolver, TemplateId, TemplateInfo, Validate,
//! };
//!
//! /// A custom proposal payload, defined outside the crate.
//! struct SetNote {
//!     note: String,
//! }
//!
//! impl TemplateInfo for SetNote {
//!     fn template_id(&self, _pkgs: &dyn PackageResolver) -> Result<TemplateId, Error> {
//!         Ok(TemplateId::new("#my-pkg", "My.Module", "SetNote"))
//!     }
//! }
//!
//! impl DamlProtoEncode for SetNote {
//!     fn to_daml_proto(&self) -> Result<Value, Error> {
//!         Ok(make_record(vec![field("note", make_text(&self.note))]))
//!     }
//! }
//!
//! impl Validate for SetNote {} // default body: accept
//!
//! let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
//! let member: CantonId = format!("member::{ns}").parse().expect("valid canton id");
//! let governance: CantonId = format!("gov::{ns}").parse().expect("valid canton id");
//!
//! // Propose: any GrpcPayload flows through the one builder. A bare `&str`
//! // is a `PackageResolver` too — every key resolves to it, which is enough
//! // when an integrator's templates all live in one package.
//! let payload = SetNote { note: "hi".into() };
//! let commands = build_propose(&payload, &governance, &member, &"#fake-pkg", "cmd-1".into())
//!     .expect("propose commands build");
//! assert_eq!(commands.act_as, vec![member.to_string()]);
//!
//! // Read side: a custom action's confirmations are `GovernanceConfirmation`
//! // contracts, which carry no inline action. Decode their created events
//! // with `parse_domain_confirmation`. (`parse_confirmation` reads only the
//! // CBTC and self shapes, which embed the action, and returns `None` for
//! // these.)
//! let _ = parse_domain_confirmation(&Default::default());
//!
//! // By cid: once a proposal's contract id is known (e.g. from
//! // `first_created_contract_id` on the propose transaction), confirm it
//! // directly, no re-decoding required.
//! let rules = TemplateId::new("#governance-core", "Governance.Rules", "GovernanceRules");
//! let _ = build_confirm_proposal(&rules, "rules-cid", &member, &governance, "proposal-cid", "cmd-2".into());
//! ```

pub mod catalog;
pub mod error;
pub mod framework;

pub use error::Error;

// These crates appear in the public signatures. The re-exports let an
// integrator name their types without a direct dependency pinned to the
// same git revision. `common` is the decman server's own crate, so the
// block re-exports only the types that the lib's signatures use, never
// the server's HTTP surface.
pub use canton_common;
pub use canton_proto_rs;
pub use common::{
    api::{
        Claim, InstrumentAllowance, InstrumentId, InstrumentIdentifier,
        InstrumentIssuerCredentials, PackageConfig, PartyCredentialRequirement, RequiredClaim,
    },
    canton_id::CantonId,
};
