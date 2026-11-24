//! Party token, credential and threshold lookups shared by the endpoints.

use actix_web::web;
use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    BaseQuery, ListDecentralizedNamespaceDefinitionRequest, StoreId, Synchronizer, base_query,
    store_id, synchronizer, topology_manager_read_service_client::TopologyManagerReadServiceClient,
};
#[cfg(test)]
use tokio::sync::Mutex;

use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{PackageConfig, default_package_config},
    error::Result,
    server::{AppState, error::ApiError},
    utils,
};

// ============================================================================
// Helper Functions
// ============================================================================

/// Get token for a party from auth registry
pub(crate) async fn get_party_token(
    data: &web::Data<AppState>,
    party_id: &CantonId,
) -> Option<String> {
    let auth = data.auth.read().await;
    match &*auth {
        Some(WorkflowAuth::Keycloak(registry)) => registry.get(party_id)?.get_token().await.ok(),
        Some(WorkflowAuth::Mock(mock_registry)) => {
            Some(mock_registry.get(party_id).await.get_token())
        }
        None => None,
    }
}

/// Get threshold for a decentralized party
pub(super) async fn get_party_threshold(
    data: &web::Data<AppState>,
    party_id: &CantonId,
) -> Option<usize> {
    let namespace = party_id.namespace.to_hex();

    let channel = data.config.admin_channel().await.ok()?;

    let mut topology_client = TopologyManagerReadServiceClient::new(channel)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let synchronizer_id = utils::get_synchronizer_id(&data.config).await.ok()?;

    let response = topology_client
        .list_decentralized_namespace_definition(tonic::Request::new(
            ListDecentralizedNamespaceDefinitionRequest {
                base_query: Some(BaseQuery {
                    store: Some(StoreId {
                        store: Some(store_id::Store::Synchronizer(Synchronizer {
                            kind: Some(synchronizer::Kind::PhysicalId(synchronizer_id)),
                        })),
                    }),
                    proposals: false,
                    operation: 0,
                    time_query: Some(base_query::TimeQuery::HeadState(())),
                    filter_signed_key: String::new(),
                    protocol_version: None,
                    client_version: None,
                }),
                filter_namespace: namespace,
            },
        ))
        .await
        .ok()?
        .into_inner();

    response
        .results
        .first()
        .and_then(|r| r.item.as_ref())
        .map(|item| item.threshold as usize)
}

/// Get member_party_id for a decentralized party from config
pub(super) async fn get_member_party_id(
    data: &web::Data<AppState>,
    party_id: &CantonId,
) -> Option<CantonId> {
    let party_creds = data.party_credentials.read().await;
    party_creds
        .iter()
        .find(|p| &p.dec_party_id == party_id)
        .map(|p| p.member_party_id.clone())
}

/// Get token and member_party_id for a party.
///
/// `Ok(None)`: no auth configured, or the party is unknown to the registry.
/// `Err`: the party is known, but the token fetch failed (logged here).
pub(crate) async fn get_party_credentials(
    data: &web::Data<AppState>,
    party_id: &CantonId,
) -> Result<Option<(String, CantonId)>> {
    let auth = data.auth.read().await;
    match &*auth {
        Some(WorkflowAuth::Keycloak(registry)) => {
            let Some(tm) = registry.get(party_id) else {
                return Ok(None);
            };
            match tm.get_token().await {
                Ok(token) => Ok(Some((token, tm.member_party_id().clone()))),
                Err(e) => {
                    tracing::warn!(%party_id, error = %e, "failed to fetch auth token for party");
                    Err(e.into())
                }
            }
        }
        Some(WorkflowAuth::Mock(mock_registry)) => {
            let mm = mock_registry.get(party_id).await;
            Ok(Some((mm.get_token(), mm.member_party_id().clone())))
        }
        None => Ok(None),
    }
}

/// A party's ledger token and the member party it acts as.
pub(super) struct PartySession {
    pub(super) token: String,
    pub(super) member_party_id: CantonId,
}

impl PartySession {
    /// Look up `party_id`'s credentials. A party without credentials is a
    /// 404, and a failed token fetch is a 500.
    pub(super) async fn load(
        data: &web::Data<AppState>,
        party_id: &CantonId,
    ) -> Result<Self, ApiError> {
        match get_party_credentials(data, party_id).await {
            Ok(Some((token, member_party_id))) => Ok(Self {
                token,
                member_party_id,
            }),
            Ok(None) => Err(ApiError::NotFound(
                "No credentials configured for party".to_string(),
            )),
            Err(e) => Err(ApiError::Internal(format!(
                "Failed to fetch auth token for party: {e}"
            ))),
        }
    }
}

/// Get the hardcoded default package config (package IDs are constants, not per-party)
pub(crate) fn packages() -> PackageConfig {
    default_package_config()
}

/// Held by each test that makes a token fetch fail. A failed fetch logs a
/// warning, and one test captures that warning with a thread-local
/// subscriber; the same log line hit at once from another test's thread can
/// hide it from the capture.
#[cfg(test)]
static TOKEN_FAILURE: Mutex<()> = Mutex::const_new(());

#[cfg(test)]
mod get_party_credentials_tests {
    use std::{
        io,
        io::Write,
        sync::{Arc, Mutex as StdMutex},
    };

    use tracing_subscriber::fmt::MakeWriter;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path_regex},
    };

    use super::*;
    use crate::{
        auth::{AuthRegistry, WorkflowAuth},
        config::{KeycloakConfig, PartyCredentials},
    };

    #[derive(Clone, Default)]
    struct LogBuffer(Arc<StdMutex<Vec<u8>>>);

    impl Write for LogBuffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut written = self
                .0
                .lock()
                .map_err(|_| io::Error::other("log buffer lock poisoned"))?;
            written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for LogBuffer {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    impl LogBuffer {
        fn captured(&self) -> Result<String> {
            let written = self
                .0
                .lock()
                .map_err(|_| anyhow::anyhow!("log buffer lock poisoned"))?
                .clone();
            Ok(String::from_utf8(written)?)
        }
    }

    fn party_id() -> Result<CantonId> {
        CantonId::parse(&format!("p::{}", "0".repeat(68)))
    }

    #[tokio::test]
    async fn no_auth_configured_is_ok_none() -> Result<()> {
        let data = AppState::for_test(None).await?;
        assert!(get_party_credentials(&data, &party_id()?).await?.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn unknown_party_is_ok_none() -> Result<()> {
        let registry = AuthRegistry::new(&[]).await?;
        let data = AppState::for_test(Some(WorkflowAuth::Keycloak(Arc::new(registry)))).await?;
        assert!(get_party_credentials(&data, &party_id()?).await?.is_none());
        Ok(())
    }

    /// The party authenticates fine at startup (`expires_in: 0` puts the token
    /// on the refresh path immediately), then the Keycloak stand-in fails the
    /// very next request — the one `get_party_credentials` makes.
    #[tokio::test]
    async fn token_fetch_failure_is_err_and_logged() -> Result<()> {
        let _serial = super::TOKEN_FAILURE.lock().await;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"access_token": "t", "expires_in": 0})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(serde_json::json!({"error": "down"})),
            )
            .mount(&server)
            .await;

        let id = party_id()?;
        let credentials = PartyCredentials {
            dec_party_id: id.clone(),
            member_party_id: id.clone(),
            user_id: "user".to_string(),
            keycloak: KeycloakConfig {
                url: server.uri(),
                realm: "test-realm".to_string(),
                client_id: "decman".to_string(),
                client_secret: Some("secret".to_string()),
                ..KeycloakConfig::default()
            },
            auth0: None,
            packages: PackageConfig::default(),
        };
        let registry = AuthRegistry::new(&[credentials]).await?;
        let data = AppState::for_test(Some(WorkflowAuth::Keycloak(Arc::new(registry)))).await?;

        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .finish();
        let result = {
            let _guard = tracing::subscriber::set_default(subscriber);
            get_party_credentials(&data, &id).await
        };

        assert!(
            result.is_err(),
            "a failed token fetch must be an error, not a silent None"
        );
        let logs = buffer.captured()?;
        assert!(logs.contains("WARN"), "no warning was logged: {logs}");
        assert!(
            logs.contains(&id.to_string()),
            "the log omits the party id: {logs}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod credential_answer_tests {
    //! Pins the 404 and 500 answers of every endpoint that looks up a
    //! party's credentials: the status code and the exact JSON body.

    use std::sync::Arc;

    use actix_web::{
        App, HttpMessage, dev::HttpServiceFactory, http::StatusCode, test as actix_test,
    };
    use serde_json::json;
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path_regex},
    };

    use super::*;
    use crate::{
        auth::{AuthRegistry, Principal, WorkflowAuth},
        config::{KeycloakConfig, PartyCredentials},
        server::{
            handlers::governance::{
                cancel_confirmation, cancel_proposal, confirm_action, execute_action,
                expire_confirmation, get_coupon_reassignment_delegation, get_reward_status,
                propose_action,
            },
            types::ErrorResponse,
        },
    };

    fn party_id() -> Result<CantonId> {
        CantonId::parse(&format!("p::{}", "0".repeat(68)))
    }

    /// A state whose Keycloak accepts the party once, at startup, and fails
    /// every later token request.
    async fn state_with_a_failing_token(server: &MockServer) -> Result<web::Data<AppState>> {
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "t", "expires_in": 0})),
            )
            .up_to_n_times(1)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/token$"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": "down"})))
            .mount(server)
            .await;
        let id = party_id()?;
        let credentials = PartyCredentials {
            dec_party_id: id.clone(),
            member_party_id: id,
            user_id: "user".to_string(),
            keycloak: KeycloakConfig {
                url: server.uri(),
                realm: "test-realm".to_string(),
                client_id: "decman".to_string(),
                client_secret: Some("secret".to_string()),
                ..KeycloakConfig::default()
            },
            auth0: None,
            packages: PackageConfig::default(),
        };
        let registry = AuthRegistry::new(&[credentials]).await?;
        AppState::for_test(Some(WorkflowAuth::Keycloak(Arc::new(registry)))).await
    }

    /// Call `service` as an admin, with `body` when it is a POST.
    async fn call(
        data: &web::Data<AppState>,
        service: impl HttpServiceFactory + 'static,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> Result<(StatusCode, Option<String>, String)> {
        let app =
            actix_test::init_service(App::new().app_data(data.clone()).service(service)).await;
        let request = match body {
            Some(body) => actix_test::TestRequest::post().uri(uri).set_json(body),
            None => actix_test::TestRequest::get().uri(uri),
        }
        .to_request();
        request.extensions_mut().insert(Principal {
            sub: "admin".to_string(),
            issuer: "test".to_string(),
            roles: Vec::new(),
            email: None,
        });
        let response = actix_test::call_service(&app, request).await;
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = actix_test::read_body(response).await;
        Ok((status, content_type, String::from_utf8(body.to_vec())?))
    }

    /// Ask every credentialed endpoint for `party`, and assert each answers
    /// `status` with exactly `error`.
    async fn assert_every_endpoint_answers(
        data: &web::Data<AppState>,
        status: StatusCode,
        error: &str,
    ) -> Result<()> {
        let party = party_id()?.to_string();
        let expected = serde_json::to_string(&ErrorResponse {
            error: error.to_string(),
        })?;
        let domain_action = json!({ "type": "governance_set_threshold", "new_threshold": 0 });
        let answers = [
            call(
                data,
                confirm_action,
                "/governance/confirm",
                Some(json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "action": domain_action,
                    "governance_type": "core_domain",
                    "proposal_cid": "00proposal",
                })),
            )
            .await?,
            call(
                data,
                execute_action,
                "/governance/execute",
                Some(json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "action": domain_action,
                    "confirmation_cids": ["00confirmation"],
                    "governance_type": "core_domain",
                    "proposal_cid": "00proposal",
                })),
            )
            .await?,
            call(
                data,
                expire_confirmation,
                "/governance/expire",
                Some(json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "confirmation_cid": "00confirmation",
                })),
            )
            .await?,
            call(
                data,
                cancel_confirmation,
                "/governance/cancel",
                Some(json!({ "party_id": party, "confirmation_cid": "00confirmation" })),
            )
            .await?,
            call(
                data,
                cancel_proposal,
                "/governance/cancel-proposal",
                Some(json!({ "party_id": party, "proposal_cid": "00proposal" })),
            )
            .await?,
            call(
                data,
                propose_action,
                "/governance/propose",
                Some(json!({
                    "party_id": party,
                    "rules_contract_id": "00rules",
                    "proposal": { "type": "generic_vote", "description": "a vote" },
                })),
            )
            .await?,
            call(
                data,
                get_coupon_reassignment_delegation,
                &format!("/coupon-reassignment-delegation?party_id={party}"),
                None,
            )
            .await?,
            call(
                data,
                get_reward_status,
                &format!("/reward-status?party_id={party}"),
                None,
            )
            .await?,
        ];
        for (index, (answer_status, content_type, body)) in answers.into_iter().enumerate() {
            assert_eq!(answer_status, status, "endpoint {index}: {body}");
            assert_eq!(
                content_type.as_deref(),
                Some("application/json"),
                "endpoint {index}"
            );
            assert_eq!(body, expected, "endpoint {index}");
        }
        Ok(())
    }

    #[actix_web::test]
    async fn a_party_without_credentials_is_a_404_everywhere() -> Result<()> {
        let data = AppState::for_test(None).await?;
        assert_every_endpoint_answers(
            &data,
            StatusCode::NOT_FOUND,
            "No credentials configured for party",
        )
        .await
    }

    #[actix_web::test]
    async fn a_failed_token_fetch_is_a_500_everywhere() -> Result<()> {
        let _serial = super::TOKEN_FAILURE.lock().await;
        let server = MockServer::start().await;
        let data = state_with_a_failing_token(&server).await?;
        let error = get_party_credentials(&data, &party_id()?)
            .await
            .err()
            .ok_or_else(|| anyhow::anyhow!("the token fetch must fail"))?;
        assert_every_endpoint_answers(
            &data,
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Failed to fetch auth token for party: {error}"),
        )
        .await
    }
}
