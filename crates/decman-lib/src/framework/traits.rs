use canton_proto_rs::com::daml::ledger::api::v2::Value;
use common::api::PackageConfig;
use common::canton_id::CantonId;

use crate::error::Error;
use crate::framework::TemplateId;

/// Context a payload's protocol checks may need. Time enters as a
/// parameter so validation is deterministic and testable.
pub struct ValidationCtx<'a> {
    pub governance_party: &'a CantonId,
    pub now_micros: i64,
}

/// Protocol-constraint checks, mirroring the Daml `ensure`/`require`
/// clauses. Default body accepts; no blanket impl — every payload writes
/// its own impl, or an empty impl to accept the default.
pub trait Validate {
    fn validate(&self, _ctx: &ValidationCtx) -> Result<(), Error> {
        Ok(())
    }
}

/// Encode to the Daml proto `Value` the gRPC Ledger API accepts.
///
/// Contract for proposal payloads: return the create-arguments record
/// WITHOUT the `governanceParty` and `proposer` fields. The propose
/// builder injects those two; they are runtime parties, not payload data.
pub trait DamlProtoEncode {
    fn to_daml_proto(&self) -> Result<Value, Error>;
}

/// Which template this payload creates.
pub trait TemplateInfo {
    fn template_id(&self, pkgs: &PackageConfig) -> Result<TemplateId, Error>;
}

/// Convenience combination. Frozen once published — phase 2 adds NEW
/// combinations (JsonPayload, DualPayload) instead of widening this one.
pub trait GrpcPayload: TemplateInfo + DamlProtoEncode + Validate {}
impl<T: TemplateInfo + DamlProtoEncode + Validate> GrpcPayload for T {}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::framework::encode::{field, make_record, make_text};

    pub(crate) struct FakeProposal {
        pub(crate) note: String,
    }

    impl TemplateInfo for FakeProposal {
        fn template_id(&self, _p: &PackageConfig) -> Result<TemplateId, Error> {
            Ok(TemplateId::new("#fake-pkg", "Fake.Module", "FakeProposal"))
        }
    }

    impl DamlProtoEncode for FakeProposal {
        fn to_daml_proto(&self) -> Result<Value, Error> {
            Ok(make_record(vec![field("note", make_text(&self.note))]))
        }
    }

    impl Validate for FakeProposal {}

    fn assert_grpc_payload(_: &impl GrpcPayload) {}

    #[test]
    fn external_struct_satisfies_grpc_payload_via_blanket_impl() {
        assert_grpc_payload(&FakeProposal { note: "x".into() });
    }
}
