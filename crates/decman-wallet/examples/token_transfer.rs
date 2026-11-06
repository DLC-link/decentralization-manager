//! Send and accept Token Standard transfers as an external party, from the
//! command line.
//!
//! Configured by environment variables. `MODE` picks what to do:
//!
//! - `list` (the default): the party's holdings.
//! - `pending`: the transfers waiting for the party to accept them.
//! - `send`: send `AMOUNT` of the instrument to `RECEIVER`.
//! - `accept`: accept the transfer instruction `INSTRUCTION_CID`, of the
//!   template `INSTRUCTION_TEMPLATE`.
//!
//! Every mode needs `PARTY_ID`, `LEDGER_JSON_API_URL` and `LEDGER_USER_ID`.
//! `LEDGER_TOKEN` is the optional bearer token for the Ledger API.
//!
//! `send` and `accept` also need `DECMAN_WALLET_SEED`, the party's base64
//! Ed25519 seed, and `REGISTRY_URL`, the instrument's registry root.
//! `REGISTRY_TOKEN` is optional. `send` needs `RECEIVER`, `AMOUNT`,
//! `INSTRUMENT_ADMIN` and `INSTRUMENT_ID`, and takes an optional
//! `VALIDITY_HOURS` (24 by default). `accept` needs `INSTRUCTION_CID` and
//! `INSTRUCTION_TEMPLATE`, the template the instruction must have, such as
//! `#splice-amulet:Splice.AmuletTransferInstruction:AmuletTransferInstruction`.
//! A template written as `<package-id>:Module:Entity` pins one package.
//! `pending` prints each offer's template and package id, as the participant
//! reports them.
//!
//! ```sh
//! MODE=send PARTY_ID=alice::1220… DECMAN_WALLET_SEED=… \
//!   LEDGER_JSON_API_URL=http://localhost:7575/api/json-api LEDGER_USER_ID=wallet \
//!   REGISTRY_URL=https://scan.example.com RECEIVER=bob::1220… AMOUNT=1.25 \
//!   INSTRUMENT_ADMIN=DSO::1220… INSTRUMENT_ID=Amulet \
//!   cargo run -p decman-wallet --example token_transfer
//! ```

use std::{env, error::Error, time::Duration};

use decman_wallet::{
    ExternalKeyPair, LedgerClient, RegistryClient, TransferArgs, accept_transfer, send_transfer,
};
use tracing_subscriber::EnvFilter;
use zeroize::Zeroizing;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let mode = env::var("MODE").unwrap_or_else(|_| "list".to_string());
    let party = required("PARTY_ID")?;
    let ledger = LedgerClient::new(
        required("LEDGER_JSON_API_URL")?,
        env::var("LEDGER_TOKEN").ok(),
        required("LEDGER_USER_ID")?,
    )?;

    match mode.as_str() {
        "list" => log_holdings(&ledger, &party, "holding").await,
        "pending" => log_pending(&ledger, &party).await,
        "send" => send(&ledger, &party).await,
        "accept" => accept(&ledger, &party).await,
        other => Err(format!("MODE must be list, pending, send or accept, not {other}").into()),
    }
}

async fn send(ledger: &LedgerClient, party: &str) -> Result {
    let key = key_for(party)?;
    let registry = registry()?;
    let validity_hours = match env::var("VALIDITY_HOURS") {
        Ok(hours) => hours.parse::<u64>()?,
        Err(_) => 24,
    };
    let args = TransferArgs::new(
        party,
        required("RECEIVER")?,
        required("AMOUNT")?,
        required("INSTRUMENT_ADMIN")?,
        required("INSTRUMENT_ID")?,
        Duration::from_secs(validity_hours.saturating_mul(3600)),
    );

    log_holdings(ledger, party, "holding before").await?;
    let receipt = send_transfer(ledger, &registry, &key, &args).await?;
    tracing::info!(
        factory_id = %receipt.factory_id,
        transfer_kind = %receipt.transfer_kind,
        inputs = ?receipt.input_holding_cids,
        signed_hash = %receipt.prepared_transaction_hash,
        update_id = %receipt.update_id,
        completion_offset = receipt.completion_offset,
        "sent"
    );
    log_holdings(ledger, party, "holding after").await
}

async fn accept(ledger: &LedgerClient, party: &str) -> Result {
    let key = key_for(party)?;
    let registry = registry()?;
    let instruction_cid = required("INSTRUCTION_CID")?;
    let instruction_template = required("INSTRUCTION_TEMPLATE")?;

    log_holdings(ledger, party, "holding before").await?;
    let receipt = accept_transfer(
        ledger,
        &registry,
        &key,
        party,
        &instruction_cid,
        &instruction_template,
    )
    .await?;
    tracing::info!(
        instruction_cid = %receipt.instruction_cid,
        signed_hash = %receipt.prepared_transaction_hash,
        update_id = %receipt.update_id,
        completion_offset = receipt.completion_offset,
        "accepted"
    );
    log_holdings(ledger, party, "holding after").await
}

/// The party's key, from `DECMAN_WALLET_SEED`. Refuses a key whose
/// fingerprint is not the party's: that key cannot sign for it.
fn key_for(party: &str) -> Result<ExternalKeyPair> {
    let seed = Zeroizing::new(required("DECMAN_WALLET_SEED")?);
    let key = ExternalKeyPair::from_seed_b64(&seed)?;
    let fingerprint = key.fingerprint();
    match party.rsplit_once("::") {
        Some((_, party_fingerprint)) if party_fingerprint == fingerprint => Ok(key),
        _ => Err(format!(
            "DECMAN_WALLET_SEED has fingerprint {fingerprint}, which is not the fingerprint of {party}"
        )
        .into()),
    }
}

fn registry() -> Result<RegistryClient> {
    Ok(RegistryClient::new(
        required("REGISTRY_URL")?,
        env::var("REGISTRY_TOKEN").ok(),
    )?)
}

async fn log_holdings(ledger: &LedgerClient, party: &str, label: &str) -> Result {
    let holdings = ledger.holdings(party).await?;
    if holdings.is_empty() {
        tracing::info!(party, "{label}: none");
    }
    for holding in holdings {
        tracing::info!(
            contract_id = %holding.contract_id,
            instrument_admin = %holding.instrument_admin,
            instrument_id = %holding.instrument_id,
            amount = %holding.amount,
            locked = holding.locked,
            "{label}"
        );
    }
    Ok(())
}

async fn log_pending(ledger: &LedgerClient, party: &str) -> Result {
    let pending = ledger.transfer_instructions(party).await?;
    if pending.is_empty() {
        tracing::info!(party, "no transfers wait for this party");
    }
    for instruction in pending {
        tracing::info!(
            instruction_cid = %instruction.contract_id,
            template = %instruction.template,
            package_id = %instruction.package_id,
            sender = %instruction.sender,
            instrument_id = %instruction.instrument_id,
            amount = %instruction.amount,
            execute_before = %instruction.execute_before,
            "pending transfer"
        );
    }
    Ok(())
}

fn required(name: &str) -> Result<String> {
    env::var(name).map_err(|_| format!("{name} must be set").into())
}
