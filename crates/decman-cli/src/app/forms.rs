//! Form validation and the request bodies the forms build.

use anyhow::Result;
use serde_json::{Value, json};

use crate::api::{DecmanClient, PartyConfigView, PeerEntry};

use crate::app::overlay::{DeployForm, IdpMode, PartyConfigForm, PcRow, PeerForm};

/// Validate a decentralized-party id prefix, mirroring the web frontend's
/// onboarding rules: starts with a letter, only `[A-Za-z0-9_-]`, ≤180 chars.
pub(super) fn validate_prefix(prefix: &str) -> Result<(), String> {
    match prefix.chars().next() {
        None => return Err("Party id prefix is required.".to_owned()),
        Some(c) if !c.is_ascii_alphabetic() => {
            return Err("Party id prefix must start with a letter.".to_owned());
        }
        Some(_) => {}
    }
    if prefix.chars().count() > 180 {
        return Err("Party id prefix must be 180 characters or fewer.".to_owned());
    }
    if !prefix
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Party id prefix may only contain letters, digits, '-' and '_'.".to_owned());
    }
    Ok(())
}

/// Validate the add-peer form into a [`PeerEntry`], or return the index of the
/// first invalid field so the cursor can land on it. Required: participant id
/// (0), address (2), a non-zero `u16` port (3) and public key (4); name (1) and
/// party (5) are optional.
pub(super) fn validate_peer_form(form: &PeerForm) -> Result<PeerEntry, usize> {
    if form.participant_id.trim().is_empty() {
        return Err(0);
    }
    if form.address.trim().is_empty() {
        return Err(2);
    }
    let port: u16 = match form.port.trim().parse() {
        Ok(port) if port > 0 => port,
        _ => return Err(3),
    };
    if form.public_key.trim().is_empty() {
        return Err(4);
    }
    let party = form.party.trim();
    Ok(PeerEntry {
        participant_id: form.participant_id.trim().to_owned(),
        name: form.name.trim().to_owned(),
        address: form.address.trim().to_owned(),
        port,
        public_key: form.public_key.trim().to_owned(),
        party: (!party.is_empty()).then(|| party.to_owned()),
    })
}

/// Serialize the editor's peer list as the bare JSON array `POST /network-config`
/// expects.
pub(super) fn peers_to_json(peers: &[PeerEntry]) -> Value {
    Value::Array(
        peers
            .iter()
            .map(|peer| {
                json!({
                    "participant_id": peer.participant_id,
                    "name": peer.name,
                    "address": peer.address,
                    "port": peer.port,
                    "public_key": peer.public_key,
                    "party": peer.party,
                })
            })
            .collect(),
    )
}

/// Fetch a party's IdP configuration and build the editor form, defaulting to
/// the Auth0 tab when an Auth0 domain is already configured.
pub(super) fn build_party_config_form(
    client: &mut DecmanClient,
    dec_party_id: &str,
) -> Result<PartyConfigForm> {
    let config = client.fetch_party_config(dec_party_id)?;
    let PartyConfigView {
        member_party_id,
        user_id,
        keycloak_url,
        keycloak_realm,
        keycloak_client_id,
        has_client_secret,
        auth0_domain,
        auth0_audience,
        auth0_client_id,
        has_auth0_client_secret,
    } = config;
    let mode = if auth0_domain.as_deref().is_some_and(|d| !d.is_empty()) {
        IdpMode::Auth0
    } else {
        IdpMode::Keycloak
    };
    Ok(PartyConfigForm {
        dec_party_id: dec_party_id.to_owned(),
        party_name: dec_party_id
            .split("::")
            .next()
            .unwrap_or(dec_party_id)
            .to_owned(),
        mode,
        member_party_id: member_party_id.unwrap_or_default(),
        user_id: user_id.unwrap_or_default(),
        kc_url: keycloak_url,
        kc_realm: keycloak_realm,
        kc_client_id: keycloak_client_id,
        kc_secret: String::new(),
        kc_has_secret: has_client_secret,
        auth0_domain: auth0_domain.unwrap_or_default(),
        auth0_audience: auth0_audience.unwrap_or_default(),
        auth0_client_id: auth0_client_id.unwrap_or_default(),
        auth0_secret: String::new(),
        auth0_has_secret: has_auth0_client_secret,
        cursor: 0,
        status: String::new(),
    })
}

/// Build the `PUT /party-config` body from the form. A blank secret is omitted
/// (keep existing); the inactive provider's secret is never sent.
pub(super) fn party_config_body(form: &PartyConfigForm) -> Value {
    let mut body = json!({
        "dec_party_id": form.dec_party_id,
        "member_party_id": form.member_party_id.trim(),
        "user_id": form.user_id.trim(),
    });
    match form.mode {
        IdpMode::Keycloak => {
            body["keycloak_url"] = json!(form.kc_url.trim());
            body["keycloak_realm"] = json!(form.kc_realm.trim());
            body["keycloak_client_id"] = json!(form.kc_client_id.trim());
            if !form.kc_secret.is_empty() {
                body["keycloak_client_secret"] = json!(form.kc_secret);
            }
        }
        IdpMode::Auth0 => {
            body["auth0_domain"] = json!(form.auth0_domain.trim());
            body["auth0_audience"] = json!(form.auth0_audience.trim());
            body["auth0_client_id"] = json!(form.auth0_client_id.trim());
            if !form.auth0_secret.is_empty() {
                body["auth0_client_secret"] = json!(form.auth0_secret);
            }
        }
    }
    body
}

/// Build the discover-member-party body from the form's active provider fields.
/// A blank secret is omitted entirely (the server models it as `Option`, so an
/// empty string would deserialize to `Some("")` and break authentication).
pub(super) fn discover_body(form: &PartyConfigForm) -> Value {
    match form.mode {
        IdpMode::Keycloak => {
            let mut body = json!({
                "keycloak_url": form.kc_url.trim(),
                "keycloak_realm": form.kc_realm.trim(),
                "keycloak_client_id": form.kc_client_id.trim(),
            });
            if !form.kc_secret.is_empty() {
                body["keycloak_client_secret"] = json!(form.kc_secret);
            }
            body
        }
        IdpMode::Auth0 => {
            let mut body = json!({
                "auth0_domain": form.auth0_domain.trim(),
                "auth0_audience": form.auth0_audience.trim(),
                "auth0_client_id": form.auth0_client_id.trim(),
            });
            if !form.auth0_secret.is_empty() {
                body["auth0_client_secret"] = json!(form.auth0_secret);
            }
            body
        }
    }
}

/// The editable string for a party-config form row, or `None` for the
/// provider toggle and the action rows.
pub(super) fn pc_field_mut(form: &mut PartyConfigForm, row: Option<PcRow>) -> Option<&mut String> {
    match row? {
        PcRow::MemberParty => Some(&mut form.member_party_id),
        PcRow::UserId => Some(&mut form.user_id),
        PcRow::KcUrl => Some(&mut form.kc_url),
        PcRow::KcRealm => Some(&mut form.kc_realm),
        PcRow::KcClientId => Some(&mut form.kc_client_id),
        PcRow::KcSecret => Some(&mut form.kc_secret),
        PcRow::Auth0Domain => Some(&mut form.auth0_domain),
        PcRow::Auth0Audience => Some(&mut form.auth0_audience),
        PcRow::Auth0ClientId => Some(&mut form.auth0_client_id),
        PcRow::Auth0Secret => Some(&mut form.auth0_secret),
        PcRow::Mode | PcRow::Discover | PcRow::Submit => None,
    }
}

/// Build the `POST /contracts` body for a governance-core deployment, or a
/// human error describing why it cannot be submitted.
pub(super) fn build_contracts_request(form: &DeployForm) -> Result<Value, String> {
    if form.package_id.is_empty() {
        return Err(
            "Unknown governance-core package — vet the governance-core DAR first.".to_owned(),
        );
    }
    if form.member_parties.is_empty() {
        return Err("No governance members resolved — peers may be unreachable.".to_owned());
    }
    let threshold: i64 = form
        .threshold
        .trim()
        .parse()
        .map_err(|_| "Threshold must be a whole number".to_owned())?;
    let timeout: i64 = form
        .timeout_micros
        .trim()
        .parse()
        .map_err(|_| "Timeout must be a whole number".to_owned())?;
    // Validate ranges client-side so the error is immediate, not a chain failure.
    let member_count = i64::try_from(form.member_parties.len()).unwrap_or(i64::MAX);
    if !(1..=member_count).contains(&threshold) {
        return Err(format!("Threshold must be between 1 and {member_count}."));
    }
    if timeout <= 0 {
        return Err("Timeout must be a positive number of microseconds.".to_owned());
    }
    Ok(json!({
        "decentralized_party_id": form.party_id,
        "participant_ids": form.participant_ids,
        "participant_parties": form.member_parties,
        "operator_party": form.operator_party,
        "contracts": [{
            "id": "create-governance-rules",
            "name": "GovernanceRules",
            "package_id": form.package_id,
            "module_name": "Governance.Rules",
            "entity_name": "GovernanceRules",
            "fields": [
                { "type": "decentralized_party" },
                { "type": "party_set", "parties": form.member_parties },
                { "type": "governance_threshold", "value": threshold },
                { "type": "rel_time", "microseconds": timeout },
                { "type": "optional", "inner": { "type": "party_set", "parties": [] } }
            ]
        }]
    }))
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn validate_prefix_accepts_valid_and_rejects_invalid() {
        assert!(validate_prefix("treasury-rc5").is_ok());
        assert!(validate_prefix("a_b-9").is_ok());
        assert!(validate_prefix("").is_err()); // empty
        assert!(validate_prefix("9abc").is_err()); // must start with a letter
        assert!(validate_prefix("ab cd").is_err()); // space
        assert!(validate_prefix("ab.cd").is_err()); // illegal char
        assert!(validate_prefix(&"a".repeat(181)).is_err()); // too long
        assert!(validate_prefix(&"a".repeat(180)).is_ok()); // at the limit
    }

    fn deploy_form() -> DeployForm {
        DeployForm {
            party_id: "dec::1220".to_owned(),
            party_name: "cbtc-network".to_owned(),
            package_id: "#governance-core-v1".to_owned(),
            operator_party: "op::1220".to_owned(),
            participant_ids: vec!["p1::1220".to_owned(), "p2::1220".to_owned()],
            member_parties: vec!["m1::1220".to_owned(), "m2::1220".to_owned()],
            threshold: "2".to_owned(),
            timeout_micros: "86400000000".to_owned(),
            cursor: 0,
        }
    }

    #[test]
    fn build_contracts_request_assembles_gov_core() {
        let body = match build_contracts_request(&deploy_form()) {
            Ok(body) => body,
            Err(error) => panic!("build failed: {error}"),
        };
        assert_eq!(body["decentralized_party_id"], "dec::1220");
        assert_eq!(body["participant_ids"][0], "p1::1220");
        assert_eq!(body["participant_parties"][1], "m2::1220");
        let contract = &body["contracts"][0];
        assert_eq!(contract["entity_name"], "GovernanceRules");
        assert_eq!(contract["fields"][1]["type"], "party_set");
        assert_eq!(contract["fields"][2]["type"], "governance_threshold");
        assert_eq!(contract["fields"][2]["value"], 2);
        assert_eq!(contract["fields"][3]["microseconds"], 86_400_000_000_i64);
    }

    #[test]
    fn build_contracts_request_rejects_missing_package() {
        let mut form = deploy_form();
        form.package_id = String::new();
        match build_contracts_request(&form) {
            Err(error) => assert!(error.contains("governance-core")),
            Ok(_) => panic!("expected a missing-package error"),
        }
    }

    #[test]
    fn build_contracts_request_rejects_out_of_range_threshold() {
        // Two members, so a threshold of 3 is out of the 1..=2 range.
        let mut form = deploy_form();
        form.threshold = "3".to_owned();
        match build_contracts_request(&form) {
            Err(error) => assert!(error.contains("between 1 and 2")),
            Ok(_) => panic!("expected an out-of-range threshold error"),
        }

        let mut form = deploy_form();
        form.timeout_micros = "0".to_owned();
        match build_contracts_request(&form) {
            Err(error) => assert!(error.contains("positive")),
            Ok(_) => panic!("expected a non-positive timeout error"),
        }
    }

    #[test]
    fn validate_peer_form_rejects_blank_and_zero_port() {
        let full = PeerForm {
            participant_id: "alpha::1220".to_owned(),
            name: "alpha".to_owned(),
            address: "10.0.0.1".to_owned(),
            port: "9001".to_owned(),
            public_key: "abcd".to_owned(),
            party: String::new(),
            cursor: 0,
        };
        match validate_peer_form(&full) {
            Ok(peer) => assert_eq!(peer.port, 9001),
            Err(field) => panic!("expected a valid peer, got invalid field {field}"),
        }

        // Blank port → cursor should land on the port field (index 3).
        let mut blank_port = full;
        blank_port.port = String::new();
        assert!(matches!(validate_peer_form(&blank_port), Err(3)));

        // Missing participant id → field 0.
        let mut no_id = PeerForm::blank();
        no_id.address = "10.0.0.1".to_owned();
        no_id.port = "9001".to_owned();
        no_id.public_key = "abcd".to_owned();
        assert!(matches!(validate_peer_form(&no_id), Err(0)));
    }

    fn party_config_form() -> PartyConfigForm {
        PartyConfigForm {
            dec_party_id: "dec::1220".to_owned(),
            party_name: "dec".to_owned(),
            mode: IdpMode::Keycloak,
            member_party_id: "member::1220".to_owned(),
            user_id: "user-1".to_owned(),
            kc_url: "https://kc".to_owned(),
            kc_realm: "realm".to_owned(),
            kc_client_id: "client".to_owned(),
            kc_secret: String::new(),
            kc_has_secret: true,
            auth0_domain: String::new(),
            auth0_audience: String::new(),
            auth0_client_id: String::new(),
            auth0_secret: String::new(),
            auth0_has_secret: false,
            cursor: 0,
            status: String::new(),
        }
    }

    #[test]
    fn party_config_body_keycloak_omits_blank_secret() {
        let body = party_config_body(&party_config_form());
        assert_eq!(body["member_party_id"], "member::1220");
        assert_eq!(body["keycloak_url"], "https://kc");
        assert_eq!(body["keycloak_client_id"], "client");
        // A blank secret is omitted (the server keeps the existing one).
        assert!(body.get("keycloak_client_secret").is_none());
        // Auth0 fields are not sent while in Keycloak mode.
        assert!(body.get("auth0_domain").is_none());
    }

    #[test]
    fn party_config_body_sends_typed_secret_and_auth0_fields() {
        let mut form = party_config_form();
        form.kc_secret = "s3cr3t".to_owned();
        assert_eq!(party_config_body(&form)["keycloak_client_secret"], "s3cr3t");

        let mut auth0 = party_config_form();
        auth0.mode = IdpMode::Auth0;
        auth0.auth0_domain = "tenant.auth0.com".to_owned();
        auth0.auth0_client_id = "a0client".to_owned();
        let body = party_config_body(&auth0);
        assert_eq!(body["auth0_domain"], "tenant.auth0.com");
        assert_eq!(body["auth0_client_id"], "a0client");
        assert!(body.get("keycloak_client_secret").is_none());
    }
}
