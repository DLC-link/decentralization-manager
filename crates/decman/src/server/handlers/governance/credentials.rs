//! Party token, credential and threshold lookups shared by the endpoints.

use actix_web::web;

use canton_proto_rs::com::digitalasset::canton::topology::admin::v30::{
    BaseQuery, ListDecentralizedNamespaceDefinitionRequest, StoreId, Synchronizer, base_query,
    store_id, synchronizer, topology_manager_read_service_client::TopologyManagerReadServiceClient,
};

use crate::{
    auth::WorkflowAuth,
    canton_id::CantonId,
    config::{PackageConfig, default_package_config},
    error::Result,
    server::AppState,
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

/// Get the hardcoded default package config (package IDs are constants, not per-party)
pub(crate) fn packages() -> PackageConfig {
    default_package_config()
}

#[cfg(test)]
mod get_party_credentials_tests {
    use std::sync::{Arc, Mutex as StdMutex};

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

    impl std::io::Write for LogBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let mut written = self
                .0
                .lock()
                .map_err(|_| std::io::Error::other("log buffer lock poisoned"))?;
            written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
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
