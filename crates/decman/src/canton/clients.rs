use canton_proto_rs::com::{
    daml::ledger::api::v2::{
        admin::{
            party_management_service_client::PartyManagementServiceClient,
            user_management_service_client::UserManagementServiceClient,
        },
        command_service_client::CommandServiceClient,
        event_query_service_client::EventQueryServiceClient,
        interactive::interactive_submission_service_client::InteractiveSubmissionServiceClient,
        state_service_client::StateServiceClient,
        update_service_client::UpdateServiceClient,
    },
    digitalasset::canton::topology::admin::v30::{
        GetIdRequest, identity_initialization_service_client::IdentityInitializationServiceClient,
    },
};

use crate::{canton_id::CantonId, config::NodeConfig, error::Result};

/// Get participant ID from Canton
///
/// Queries the participant's identity initialization service to get the unique participant ID.
pub async fn get_participant_id(config: &NodeConfig) -> Result<CantonId> {
    let mut id_client = IdentityInitializationServiceClient::new(config.admin_channel().await?);
    let response = id_client
        .get_id(tonic::Request::new(GetIdRequest {}))
        .await?
        .into_inner();

    if response.unique_identifier.is_empty() {
        anyhow::bail!("No participant ID returned");
    }

    CantonId::parse(&response.unique_identifier)
}

/// Resolve the participant ID from Canton if not already set in the config.
///
/// If the participant_id is not set, queries Canton Admin API and stores
/// the result in memory. The ID is not persisted — it will be re-queried
/// on every startup.
pub async fn resolve_participant_id(config: &mut NodeConfig) -> Result {
    if config.has_participant_id() {
        tracing::debug!(
            "Participant ID already configured: {}",
            config.participant_id()
        );
        return Ok(());
    }

    tracing::info!("Participant ID not configured, querying Canton...");
    let participant_id = get_participant_id(config).await?;
    tracing::info!("Got participant ID from Canton: {participant_id}");

    config.node.participant_id = Some(participant_id);
    Ok(())
}

/// Max gRPC message size (512MB) - Canton ledger can return large responses
pub const MAX_GRPC_MESSAGE_SIZE: usize = 512 * 1024 * 1024;

/// Macro to define authenticated gRPC client creator functions
macro_rules! define_client_creator {
    ($fn_name:ident, $client_type:ident) => {
        pub async fn $fn_name(
            config: &NodeConfig,
            token: Option<String>,
        ) -> Result<
            $client_type<
                tonic::service::interceptor::InterceptedService<
                    tonic::transport::Channel,
                    impl Fn(
                        tonic::Request<()>,
                    ) -> std::result::Result<tonic::Request<()>, tonic::Status>
                    + Clone,
                >,
            >,
        > {
            let channel = config.ledger_channel().await?;

            let interceptor = move |mut req: tonic::Request<()>| {
                if let Some(ref token) = token {
                    let bearer_token = format!("Bearer {token}");
                    let token_value = tonic::metadata::MetadataValue::try_from(&bearer_token)
                        .map_err(|e| {
                            tonic::Status::unauthenticated(format!("Invalid token: {e}"))
                        })?;
                    req.metadata_mut().insert("authorization", token_value);
                }
                Ok(req)
            };

            Ok($client_type::with_interceptor(channel, interceptor)
                .max_decoding_message_size(MAX_GRPC_MESSAGE_SIZE))
        }
    };
}

define_client_creator!(create_party_client, PartyManagementServiceClient);
define_client_creator!(create_user_client, UserManagementServiceClient);
define_client_creator!(create_submission_client, InteractiveSubmissionServiceClient);
define_client_creator!(create_state_client, StateServiceClient);
define_client_creator!(create_update_client, UpdateServiceClient);
define_client_creator!(create_event_query_client, EventQueryServiceClient);
define_client_creator!(create_command_client, CommandServiceClient);
