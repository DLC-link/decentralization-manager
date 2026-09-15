//! The Ledger API client bound to the node identity.
//!
//! Every write goes through `CommandService.submit_and_wait_for_transaction`
//! with `act_as = read_as = [node party]`, the same envelope the governance
//! handlers use with the member party. Every read is a decoded ACS walk as
//! the node party, so only contracts that name the node party as signatory
//! or observer are visible, which is the whole visibility model.

use anyhow::{Context, Result, anyhow};
use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, CreateCommand, ExerciseCommand, Record, SubmitAndWaitForTransactionRequest, Value,
    command, command_service_client::CommandServiceClient,
};
use common::canton_id::CantonId;
use decman_lib::framework::commands::{commands_envelope, first_created_contract_id};

use crate::{
    config::NodeConfig,
    server::reward_automation::{ContractFilter, Entity, Module, for_each_active_created},
    utils,
};

use super::{
    codec::TemplateRecord,
    templates::{CoordinationPackage, CoordinationTemplate},
};
use crate::onledger::identity::NodeIdentity;

/// What one exercise produced.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExerciseOutcome {
    /// The first contract the choice created, when it created one. `None`
    /// for choices that return `()` (cancel, retire, archive, close).
    pub created_contract_id: Option<String>,
    /// The ledger update id, for logs.
    pub update_id: String,
}

/// One active contract of a coordination template, decoded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveContract<T> {
    pub contract_id: String,
    /// The created-event offset; orders contracts by creation.
    pub offset: i64,
    pub record: T,
}

/// Submits and reads as the node party.
#[derive(Clone)]
pub struct CoordinationClient {
    config: NodeConfig,
    identity: NodeIdentity,
    package: CoordinationPackage,
    /// Insecure/test mode: the mock token lacks `TemplateFilter` permission,
    /// so ACS reads use a wildcard filter and match in memory.
    test_mode: bool,
}

impl CoordinationClient {
    pub fn new(
        config: NodeConfig,
        identity: NodeIdentity,
        package: CoordinationPackage,
        test_mode: bool,
    ) -> Self {
        Self {
            config,
            identity,
            package,
            test_mode,
        }
    }

    pub fn node_party(&self) -> &CantonId {
        &self.identity.node_party
    }

    pub fn participant_id(&self) -> &CantonId {
        &self.identity.participant_id
    }

    pub fn identity(&self) -> &NodeIdentity {
        &self.identity
    }

    pub fn package(&self) -> &CoordinationPackage {
        &self.package
    }

    pub fn config(&self) -> &NodeConfig {
        &self.config
    }

    pub fn test_mode(&self) -> bool {
        self.test_mode
    }

    /// Create a contract from a typed record and return its contract id.
    ///
    /// # Errors
    /// Returns an error when the token cannot be minted, the ledger rejects
    /// the command, or the transaction carries no created event.
    pub async fn create<T: TemplateRecord>(&self, record: &T) -> Result<String> {
        self.create_record(T::TEMPLATE, record.to_record()).await
    }

    /// Create a contract from raw create arguments and return its contract id.
    ///
    /// # Errors
    /// As [`Self::create`].
    pub async fn create_record(
        &self,
        template: CoordinationTemplate,
        arguments: Record,
    ) -> Result<String> {
        let cmd = Command {
            command: Some(command::Command::Create(CreateCommand {
                template_id: Some(template.identifier(&self.package)),
                create_arguments: Some(arguments),
            })),
        };
        let outcome = self
            .submit(vec![cmd])
            .await
            .with_context(|| format!("create {}", template.entity()))?;
        outcome.created_contract_id.ok_or_else(|| {
            anyhow!(
                "create {} committed (update {}) but the transaction carried no created event",
                template.entity(),
                outcome.update_id
            )
        })
    }

    /// Exercise a choice as the node party.
    ///
    /// # Errors
    /// Returns an error when the token cannot be minted or the ledger rejects
    /// the command.
    pub async fn exercise(
        &self,
        template: CoordinationTemplate,
        contract_id: &str,
        choice: &str,
        argument: Value,
    ) -> Result<ExerciseOutcome> {
        let cmd = Command {
            command: Some(command::Command::Exercise(ExerciseCommand {
                template_id: Some(template.identifier(&self.package)),
                contract_id: contract_id.to_string(),
                choice: choice.to_string(),
                choice_argument: Some(argument),
            })),
        };
        self.submit(vec![cmd])
            .await
            .with_context(|| format!("exercise {choice} on {contract_id}"))
    }

    /// Visit every active contract of `template` visible to the node party,
    /// one at a time, as `(contract_id, offset, &Record)`.
    ///
    /// # Errors
    /// Returns an error when the read fails or `visit` returns one.
    pub async fn for_each_active<F>(&self, template: CoordinationTemplate, visit: F) -> Result<()>
    where
        F: FnMut(&str, i64, &Record) -> Result<()>,
    {
        let token = self.identity.token().await?;
        let filter = ContractFilter::template(
            self.package.reference(),
            Module(template.module()),
            Entity(template.entity()),
        );
        for_each_active_created(
            &self.config,
            &self.identity.node_party,
            Some(token),
            self.test_mode,
            filter,
            visit,
        )
        .await
        .with_context(|| format!("read active {}", template.entity()))
    }

    /// Every active contract of `T` visible to the node party, decoded.
    ///
    /// A contract that fails to decode is logged and skipped: one malformed
    /// contract from another node must not blind this node to the rest.
    ///
    /// # Errors
    /// Returns an error when the read itself fails.
    pub async fn list_active<T: TemplateRecord>(&self) -> Result<Vec<ActiveContract<T>>> {
        let mut out = Vec::new();
        self.for_each_active(T::TEMPLATE, |cid, offset, rec| {
            match T::from_record(rec) {
                Ok(record) => out.push(ActiveContract {
                    contract_id: cid.to_string(),
                    offset,
                    record,
                }),
                Err(e) => tracing::warn!(
                    template = T::TEMPLATE.entity(),
                    contract_id = cid,
                    error = %e,
                    "skipping a coordination contract that does not decode"
                ),
            }
            Ok(())
        })
        .await?;
        Ok(out)
    }

    async fn submit(&self, commands: Vec<Command>) -> Result<ExerciseOutcome> {
        let token = self.identity.token().await?;
        let envelope = commands_envelope(
            uuid::Uuid::new_v4().to_string(),
            &self.identity.node_party,
            &self.identity.node_party,
            commands,
            vec![],
        );
        let channel = self.config.ledger_channel().await?;
        let mut client = CommandServiceClient::new(channel)
            .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
        let mut req = tonic::Request::new(SubmitAndWaitForTransactionRequest {
            commands: Some(envelope),
            transaction_format: None,
        });
        let auth = format!("Bearer {token}")
            .parse()
            .context("authorization header is not a valid metadata value")?;
        req.metadata_mut().insert("authorization", auth);
        let response = client
            .submit_and_wait_for_transaction(req)
            .await?
            .into_inner();
        let transaction = response.transaction.as_ref();
        Ok(ExerciseOutcome {
            created_contract_id: transaction.and_then(first_created_contract_id),
            update_id: transaction.map(|t| t.update_id.clone()).unwrap_or_default(),
        })
    }
}
