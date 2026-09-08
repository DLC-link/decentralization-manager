//! Metadata encoding for the interactive-submission hashing scheme.
//!
//! Port of Canton's `v2` / `v3` `TransactionMetadataHasher`. Everything in the
//! metadata is signed: the fields that describe the ledger change, and the
//! Canton-protocol fields (mediator group, synchronizer, time bounds) that
//! decide where and when the change may be committed.

use canton_proto_rs::com::daml::ledger::api::v2::interactive::{
    Metadata, metadata::InputContract, metadata::input_contract::Contract,
};

use crate::{
    canton_hash::{
        HashingScheme,
        encoding::{Encoder, required},
        nodes::NodeHasher,
    },
    error::Result,
};

/// V2 prefixes the metadata with the version of the protobuf used to encode
/// it; V3 dropped the prefix.
const METADATA_ENCODING_V1: u8 = 0x01;

pub(super) fn hash_metadata(scheme: HashingScheme, metadata: &Metadata) -> Result<[u8; 32]> {
    let mut encoder = Encoder::with_purpose();
    if scheme == HashingScheme::V2 {
        encoder.byte(METADATA_ENCODING_V1);
    }

    let submitter = required(metadata.submitter_info.as_ref(), "submitter info")?;
    encoder.string_set(&submitter.act_as)?;
    encoder.string(&submitter.command_id)?;
    encoder.string(&metadata.transaction_uuid)?;
    let mediator_group = i32::try_from(metadata.mediator_group).map_err(|_| {
        anyhow::anyhow!(
            "mediator group {group} exceeds the int32 the encoding uses",
            group = metadata.mediator_group
        )
    })?;
    encoder.int32(mediator_group);
    encoder.string(&metadata.synchronizer_id)?;
    encode_optional_timestamp(&mut encoder, metadata.min_ledger_effective_time)?;
    encode_optional_timestamp(&mut encoder, metadata.max_ledger_effective_time)?;
    encoder.int64(timestamp(metadata.preparation_time, "preparation time")?);

    // Canton keeps the input contracts in a map keyed by contract id, so the
    // hash is over that key order regardless of how they arrived on the wire.
    let mut contracts: Vec<(&str, &InputContract)> =
        Vec::with_capacity(metadata.input_contracts.len());
    for contract in &metadata.input_contracts {
        contracts.push((contract_id(contract)?, contract));
    }
    contracts.sort_by_key(|(id, _)| *id);

    encoder.repeated(&contracts, |encoder, (_, contract)| {
        encoder.int64(timestamp(
            contract.created_at,
            "input contract creation time",
        )?);
        let Contract::V1(create) = required(contract.contract.as_ref(), "input contract")?;
        let hash = NodeHasher::hash_detached_create(scheme, create)?;
        encoder.add_hash(&hash);
        Ok(())
    })?;

    if scheme == HashingScheme::V3 {
        encode_optional_timestamp(&mut encoder, metadata.max_record_time)?;
    }

    Ok(encoder.digest())
}

fn contract_id(contract: &InputContract) -> Result<&str> {
    let Contract::V1(create) = required(contract.contract.as_ref(), "input contract")?;
    Ok(&create.contract_id)
}

/// Canton stores timestamps as microseconds in a Java `long`; the protobuf
/// widens them to `uint64`. A value that does not fit the signed range could
/// not have come from Canton.
fn timestamp(micros: u64, what: &str) -> Result<i64> {
    i64::try_from(micros)
        .map_err(|_| anyhow::anyhow!("{what} {micros} exceeds the int64 the encoding uses"))
}

fn encode_optional_timestamp(encoder: &mut Encoder, micros: Option<u64>) -> Result {
    match micros {
        Some(micros) => {
            encoder.byte(0x01);
            encoder.int64(timestamp(micros, "timestamp")?);
        }
        None => encoder.byte(0x00),
    }
    Ok(())
}
