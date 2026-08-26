//! Domain model and gRPC codecs for the decman on-chain governance protocol.
//!
//! `framework` is template-agnostic: traits, encode/record toolkits, event
//! filters, the command envelope. `catalog` is decman's protocol content:
//! the by-value `ActionType` enum, one struct per proposal, template
//! accessors, flow builders, and state interpretation.
//!
//! The crate does no I/O. Clocks, randomness, registry contexts, and
//! resolved package refs enter as parameters.
//!
//! # Quick start: a custom proposal
//!
//! An integrator adds a new proposal kind outside the crate by implementing
//! [`framework::TemplateInfo`], [`framework::DamlProtoEncode`], and
//! [`framework::Validate`] (default body accepts), then handing the payload
//! to [`framework::commands::build_propose`]:
//!
//! ```
//! use canton_proto_rs::com::daml::ledger::api::v2::Value;
//! use common::api::PackageConfig;
//! use common::canton_id::CantonId;
//! use decman_lib::Error;
//! use decman_lib::catalog::commands::build_confirm_proposal;
//! use decman_lib::catalog::interpret::parse_confirmation;
//! use decman_lib::framework::commands::build_propose;
//! use decman_lib::framework::encode::{field, make_record, make_text};
//! use decman_lib::framework::{DamlProtoEncode, TemplateId, TemplateInfo, Validate};
//!
//! /// A custom proposal payload, defined outside the crate.
//! struct SetNote {
//!     note: String,
//! }
//!
//! impl TemplateInfo for SetNote {
//!     fn template_id(&self, _pkgs: &PackageConfig) -> Result<TemplateId, Error> {
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
//! let member: CantonId = format!("member::{ns}").parse().unwrap();
//! let governance: CantonId = format!("gov::{ns}").parse().unwrap();
//!
//! // Propose: any GrpcPayload flows through the one builder.
//! let payload = SetNote { note: "hi".into() };
//! let commands =
//!     build_propose(&payload, &governance, &member, &PackageConfig::default(), "cmd-1".into())
//!         .unwrap();
//! assert_eq!(commands.act_as, vec![member.to_string()]);
//!
//! // Read side: a Ledger API `CreatedEvent` decodes to a typed
//! // `ParsedConfirmation`; an unrecognized shape comes back `None`.
//! let _ = parse_confirmation(&Default::default());
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
