//! A decentralized party shared by the parties tests.

use crate::{canton_id::CantonId, server::types::DecentralizedParty};

pub fn a_party() -> anyhow::Result<DecentralizedParty> {
    Ok(DecentralizedParty {
        party_id: CantonId::parse(
            "cbtc::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892",
        )?,
        threshold: 1,
        confirmation_threshold: None,
        owners: Vec::new(),
        my_owner_key: None,
        participants: Vec::new(),
        non_hosting_owners: Vec::new(),
        contracts: Vec::new(),
        local_metadata: None,
    })
}
