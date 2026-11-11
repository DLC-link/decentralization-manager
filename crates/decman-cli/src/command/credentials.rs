//! Party and admin credentials, and the client secrets the commands add
//! to request bodies.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::NodeSettings;
use crate::api::DecmanClient;
use crate::auth::run_secret_command;
use crate::config::{AdminClient, PartyCredentials};

/// Environment variable that holds the admin client secret for
/// `auth grant-rights`.
pub const ADMIN_SECRET_ENV: &str = "DECMAN_ADMIN_CLIENT_SECRET";

/// The profile's party credentials.
pub fn party_credentials(settings: &NodeSettings) -> Result<&PartyCredentials> {
    settings
        .party_credentials
        .as_ref()
        .context("the profile has no [profiles.party_credentials] section; pass --body instead")
}

/// The output of the party credentials' client_secret_command, for a body
/// that needs a secret.
fn party_client_secret(settings: &NodeSettings) -> Result<String> {
    let command = settings
        .party_credentials
        .as_ref()
        .and_then(|credentials| credentials.client_secret_command.as_deref())
        .context(
            "the body has no client secret, and the profile's [party_credentials] sets no \
             client_secret_command",
        )?;
    run_secret_command("party_credentials.client_secret_command", command)
}

/// The IdP fields of `credentials` that are set, as a JSON object.
pub fn credential_fields(credentials: &PartyCredentials) -> serde_json::Map<String, Value> {
    let optional = [
        ("keycloak_url", &credentials.keycloak_url),
        ("keycloak_realm", &credentials.keycloak_realm),
        ("keycloak_client_id", &credentials.keycloak_client_id),
        ("auth0_domain", &credentials.auth0_domain),
        ("auth0_audience", &credentials.auth0_audience),
        ("auth0_client_id", &credentials.auth0_client_id),
    ];
    optional
        .into_iter()
        .filter_map(|(key, value)| {
            value
                .clone()
                .map(|value| (key.to_owned(), Value::String(value)))
        })
        .collect()
}

/// Fail before any secret command runs when the profile blocks writes, so a
/// refused command never prompts the password manager.
pub fn ensure_writes_allowed(client: &DecmanClient) -> Result<()> {
    match client.writes_blocked() {
        Some(reason) => bail!("{reason}"),
        None => Ok(()),
    }
}

/// The admin client id and secret for `auth grant-rights`. The id comes from
/// `--admin-client-id`, else from the profile. The secret comes from the
/// environment, else from the profile's command. The profile's command is used
/// only for the profile's own client, so a different `--admin-client-id`
/// never gets that client's secret.
pub fn admin_credentials(
    override_id: Option<String>,
    profile: Option<&AdminClient>,
    env_secret: Option<String>,
) -> Result<(String, String)> {
    let id = override_id
        .or_else(|| profile.map(|admin| admin.client_id.clone()))
        .context("pass --admin-client-id, or add [profiles.admin_client] to the profile")?;
    if let Some(secret) = env_secret.filter(|secret| !secret.is_empty()) {
        return Ok((id, secret));
    }
    let admin = profile
        .filter(|admin| admin.client_id == id)
        .with_context(|| {
            format!(
                "set {ADMIN_SECRET_ENV} to the secret of {id}; the profile holds no command for it"
            )
        })?;
    let secret = run_secret_command(
        "admin_client.client_secret_command",
        &admin.client_secret_command,
    )?;
    Ok((id, secret))
}

/// `body` with the profile's client secret filled in, when it needs one. A
/// `--body` gets the secret only if it names the same IdP and client as the
/// profile; otherwise the server would send the secret to another IdP.
pub fn with_profile_secret(body: Value, settings: &NodeSettings) -> Result<Value> {
    with_client_secret(body, |body| {
        let credentials = party_credentials(settings)?;
        if !names_profile_client(body, credentials) {
            bail!(
                "the body names another IdP or client than the profile's [party_credentials]; \
                 put the secret in the body yourself, or pass --keep-secret"
            );
        }
        party_client_secret(settings)
    })
}

/// The IdP fields that decide where the server sends a client secret.
const IDP_FIELDS: [&str; 6] = [
    "keycloak_url",
    "keycloak_realm",
    "keycloak_client_id",
    "auth0_domain",
    "auth0_audience",
    "auth0_client_id",
];

/// `value` when it is set: not missing, not `null` and not an empty string.
fn set_value(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| !value.is_null() && *value != "")
}

/// Whether `body` names exactly the profile's IdP and client. Every IdP field
/// must match, including the fields the profile leaves unset: a body that adds
/// Auth0 fields to the profile's Keycloak fields would send the secret to Auth0.
fn names_profile_client(body: &Value, credentials: &PartyCredentials) -> bool {
    let profile = credential_fields(credentials);
    IDP_FIELDS
        .iter()
        .all(|key| set_value(body.get(*key)) == set_value(profile.get(*key)))
}

/// The `PUT /party-config` body for `party_id` from the profile's
/// credentials, without the client secret.
pub fn party_config_body(party_id: &str, credentials: &PartyCredentials) -> Value {
    let mut body = credential_fields(credentials);
    body.insert("dec_party_id".to_owned(), json!(party_id));
    body.insert(
        "member_party_id".to_owned(),
        json!(credentials.member_party_id),
    );
    body.insert("user_id".to_owned(), json!(credentials.user_id));
    Value::Object(body)
}

/// `body` with its client secret filled in by `secret`, when it needs one and
/// has none. An Auth0 body gets `auth0_client_secret`, any other body
/// `keycloak_client_secret`. A body with a Keycloak password uses the password
/// flow and gets no client secret. `secret` runs only when the body needs it.
fn with_client_secret(
    mut body: Value,
    secret: impl FnOnce(&Value) -> Result<String>,
) -> Result<Value> {
    let present = |value: &Value| !value.is_null() && value != "";
    let field = if present(&body["auth0_domain"]) {
        "auth0_client_secret"
    } else {
        "keycloak_client_secret"
    };
    if present(&body[field]) || present(&body["keycloak_password"]) {
        return Ok(body);
    }
    if !body.is_object() {
        bail!("the party config body must be a JSON object");
    }
    let secret = secret(&body)?;
    let object = body
        .as_object_mut()
        .context("the party config body must be a JSON object")?;
    object.insert(field.to_owned(), Value::String(secret));
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keycloak_body_without_a_secret_gets_one() {
        // Act
        let body = with_client_secret(json!({ "keycloak_client_id": "c" }), |_| {
            Ok("s3cret".to_owned())
        })
        .unwrap();

        // Assert
        assert_eq!(body["keycloak_client_secret"], "s3cret");
    }

    #[test]
    fn an_auth0_body_gets_the_auth0_secret_field() {
        let body = with_client_secret(json!({ "auth0_domain": "x.auth0.com" }), |_| {
            Ok("s3cret".to_owned())
        })
        .unwrap();
        assert_eq!(body["auth0_client_secret"], "s3cret");
        assert!(body.get("keycloak_client_secret").is_none());
    }

    #[test]
    fn a_body_with_a_secret_or_a_password_never_runs_the_command() {
        let never = |_: &Value| -> Result<String> { panic!("the secret command must not run") };
        let with_secret = json!({ "keycloak_client_secret": "given" });
        assert_eq!(
            with_client_secret(with_secret.clone(), never).unwrap(),
            with_secret
        );
        let with_password = json!({ "keycloak_password": "pw" });
        assert_eq!(
            with_client_secret(with_password.clone(), never).unwrap(),
            with_password
        );
    }

    #[test]
    fn a_missing_secret_without_a_profile_command_is_an_error() {
        let error = party_client_secret(&NodeSettings::default()).unwrap_err();
        assert!(
            error.to_string().contains("client_secret_command"),
            "{error}"
        );
    }

    #[test]
    fn party_config_body_comes_from_the_profile_credentials() {
        // Arrange
        let credentials = PartyCredentials {
            member_party_id: "attestor-1::1220aa".to_owned(),
            user_id: "attestor-user".to_owned(),
            keycloak_url: Some("https://idp".to_owned()),
            keycloak_realm: Some("nodes".to_owned()),
            keycloak_client_id: Some("attestor-1".to_owned()),
            auth0_domain: None,
            auth0_audience: None,
            auth0_client_id: None,
            client_secret_command: Some("never runs here".to_owned()),
        };

        // Act
        let body = party_config_body("alpha::1220bb", &credentials);

        // Assert
        assert_eq!(
            body,
            json!({
                "dec_party_id": "alpha::1220bb",
                "member_party_id": "attestor-1::1220aa",
                "user_id": "attestor-user",
                "keycloak_url": "https://idp",
                "keycloak_realm": "nodes",
                "keycloak_client_id": "attestor-1",
            })
        );
    }

    fn profile_admin() -> AdminClient {
        AdminClient {
            client_id: "participant-admin".to_owned(),
            client_secret_command: "never runs in these tests".to_owned(),
        }
    }

    fn profile_credentials() -> PartyCredentials {
        PartyCredentials {
            member_party_id: "m::1220".to_owned(),
            user_id: "u".to_owned(),
            keycloak_url: Some("https://idp".to_owned()),
            keycloak_realm: Some("nodes".to_owned()),
            keycloak_client_id: Some("attestor-1".to_owned()),
            auth0_domain: None,
            auth0_audience: None,
            auth0_client_id: None,
            client_secret_command: Some("never runs in these tests".to_owned()),
        }
    }

    #[test]
    fn the_admin_secret_from_the_environment_wins() {
        // Act
        let (id, secret) =
            admin_credentials(None, Some(&profile_admin()), Some("env-s3cret".to_owned())).unwrap();

        // Assert
        assert_eq!(id, "participant-admin");
        assert_eq!(secret, "env-s3cret");
    }

    #[test]
    fn another_admin_client_never_gets_the_profile_secret() {
        // Act
        let error = admin_credentials(
            Some("other-client".to_owned()),
            Some(&profile_admin()),
            None,
        )
        .unwrap_err();

        // Assert
        assert!(
            error.to_string().contains("secret of other-client"),
            "{error}"
        );
    }

    #[test]
    fn no_admin_client_anywhere_is_an_error() {
        let error = admin_credentials(None, None, None).unwrap_err();
        assert!(error.to_string().contains("--admin-client-id"), "{error}");
    }

    #[test]
    fn a_body_for_the_profile_client_matches() {
        let body = json!({
            "dec_party_id": "p::1220",
            "keycloak_url": "https://idp",
            "keycloak_realm": "nodes",
            "keycloak_client_id": "attestor-1",
        });
        assert!(names_profile_client(&body, &profile_credentials()));
    }

    #[test]
    fn a_body_for_another_idp_gets_no_profile_secret() {
        // Arrange — the secret would go to the IdP that the body names.
        let settings = NodeSettings {
            admin_client: None,
            party_credentials: Some(profile_credentials()),
        };
        let body = json!({
            "keycloak_url": "https://evil.example",
            "keycloak_realm": "nodes",
            "keycloak_client_id": "attestor-1",
        });

        // Act
        let error = with_profile_secret(body, &settings).unwrap_err();

        // Assert
        assert!(error.to_string().contains("another IdP"), "{error}");
    }

    #[test]
    fn a_body_that_adds_auth0_fields_gets_no_profile_secret() {
        // Arrange — the profile's Keycloak fields, plus another provider.
        let body = json!({
            "keycloak_url": "https://idp",
            "keycloak_realm": "nodes",
            "keycloak_client_id": "attestor-1",
            "auth0_domain": "evil.auth0.com",
            "auth0_client_id": "x",
        });

        // Act / Assert
        assert!(!names_profile_client(&body, &profile_credentials()));
    }
}
