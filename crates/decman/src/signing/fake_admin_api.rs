//! A fake Canton admin API for unit tests of the party key custody.
//!
//! Canton is outside this crate, so a test may stand in for it. Everything on
//! DecMan's side of the gRPC call runs for real: the channel, the requests,
//! and the handling of each reply and error.
//!
//! The fake serves `VaultService.ListMyKeys`, `GenerateSigningKey` and
//! `ExportKeyPair`, and `SynchronizerConnectivityService.GetSynchronizerId`.
//! It records each key it generates and each key it exports, so a test can
//! show what DecMan asked the vault to do.
//!
//! It also serves a synchronizer's topology head state. A test publishes the
//! namespace definitions, `PartyToParticipant` mappings and legacy
//! `PartyToKeyMapping`s it needs. The read service lists them. The write
//! service records each `Authorize` call and returns the proposed transaction
//! unsigned, and it returns each transaction sent to `SignTransactions` as it
//! came. That is what a participant that holds none of the keys would do.

use std::{
    convert::Infallible,
    sync::{Arc, Mutex, MutexGuard},
};

use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{GetSynchronizerIdRequest, GetSynchronizerIdResponse},
    crypto::{
        admin::v30::{
            ExportKeyPairRequest, ExportKeyPairResponse, GenerateSigningKeyRequest,
            GenerateSigningKeyResponse, ListMyKeysRequest, ListMyKeysResponse, PrivateKeyMetadata,
            generate_signing_key_response, private_key_metadata,
        },
        v30::{
            CryptoKeyFormat, PublicKey, PublicKeyWithName, SigningKeySpec, SigningKeyUsage,
            SigningPublicKey, public_key,
        },
    },
    protocol::v30::{
        DecentralizedNamespaceDefinition, NamespaceDelegation, PartyToKeyMapping,
        PartyToParticipant, SignedTopologyTransaction, TopologyMapping, TopologyTransaction, enums,
        topology_mapping,
    },
    topology::admin::v30::{
        AuthorizeRequest, AuthorizeResponse, BaseResult, GetIdRequest, GetIdResponse,
        ListAllRequest, ListAllResponse, ListDecentralizedNamespaceDefinitionRequest,
        ListDecentralizedNamespaceDefinitionResponse, ListNamespaceDelegationRequest,
        ListNamespaceDelegationResponse, ListPartyToKeyMappingRequest,
        ListPartyToKeyMappingResponse, ListPartyToParticipantRequest,
        ListPartyToParticipantResponse, SignTransactionsRequest, SignTransactionsResponse,
        TopologyTransactions, authorize_request, list_decentralized_namespace_definition_response,
        list_namespace_delegation_response, list_party_to_key_mapping_response,
        list_party_to_participant_response, topology_transactions,
    },
    version::v1::{UntypedVersionedMessage, untyped_versioned_message},
};
use p256::pkcs8::EncodePublicKey;
use prost::Message;
use sqlx::SqlitePool;
use tempfile::TempDir;
use tokio::task::JoinHandle;
use tonic::{
    Request, Response, Status,
    body::Body,
    codegen::{BoxFuture, Context, Poll, Service, http},
    server::{Grpc, NamedService},
    transport::{Server, server::TcpIncoming},
};

use crate::{
    canton_id::{CantonId, Fingerprint},
    config::NodeConfig,
    error::Result,
    signing::custody::SoftwareKeys,
    utils::compute_fingerprint,
    workflow::{
        onboarding::steps::generate_keys::encode_keys_payload,
        storage::{WorkflowStorage, identity_kinds},
    },
};

/// The synchronizer the fake reports, on protocol version 34.
const SYNCHRONIZER_ID: &str =
    "global-domain::1220cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc::34-0";

/// The participant id the fake reports, which [`FakeAdminApi::node`] also
/// configures.
const PARTICIPANT_ID: &str =
    "node1::1220aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// What Canton 3.7 answers an export while key download is off.
const KEY_DOWNLOAD_OFF: &str = "Remote export of private keys only allowed when explicitly enabled";

/// DER prefix of an Ed25519 `SubjectPublicKeyInfo`.
const ED25519_SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// DER prefix of an Ed25519 PKCS#8 private key, up to the seed.
const ED25519_PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// One key in the fake vault.
#[derive(Clone, Debug)]
pub(crate) struct VaultKey {
    pub name: String,
    pub public_key: SigningPublicKey,
    /// The Ed25519 seed of a software key. A KMS key has none.
    pub seed: Option<[u8; 32]>,
    pub kms_key_id: Option<String>,
    pub wrapper_key_id: Option<String>,
}

impl VaultKey {
    /// An Ed25519 key in Canton's software vault.
    pub fn software(name: &str, seed: u8) -> Self {
        let seed = [seed; 32];
        Self {
            name: name.to_string(),
            public_key: ed25519_public_key(&seed),
            seed: Some(seed),
            kms_key_id: None,
            wrapper_key_id: None,
        }
    }

    /// A software key that the vault wraps with a KMS key. It still lives in
    /// the software vault.
    pub fn wrapped(name: &str, seed: u8) -> Self {
        Self {
            wrapper_key_id: Some("arn:aws:kms:us-east-1:111122223333:key/wrapper".to_string()),
            ..Self::software(name, seed)
        }
    }

    /// A P-256 key held in AWS KMS.
    pub fn kms(name: &str, seed: u8) -> Result<Self> {
        let signing_key = p256::ecdsa::SigningKey::from_slice(&[seed; 32])?;
        let spki = signing_key.verifying_key().to_public_key_der()?;
        Ok(Self {
            name: name.to_string(),
            public_key: SigningPublicKey {
                format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
                public_key: spki.as_bytes().to_vec(),
                usage: vec![SigningKeyUsage::Protocol as i32],
                key_spec: SigningKeySpec::EcP256 as i32,
                ..Default::default()
            },
            seed: None,
            kms_key_id: Some(format!("1234abcd-12ab-34cd-56ef-1234567890{seed:02x}")),
            wrapper_key_id: None,
        })
    }

    pub fn fingerprint(&self) -> Fingerprint {
        compute_fingerprint(&self.public_key)
    }

    fn metadata(&self) -> PrivateKeyMetadata {
        PrivateKeyMetadata {
            wrapper_key_id: self.wrapper_key_id.clone(),
            kms_key_id: self.kms_key_id.clone(),
            public_key_with_name: Some(private_key_metadata::PublicKeyWithName::V30(
                PublicKeyWithName {
                    public_key: Some(PublicKey {
                        key: Some(public_key::Key::SigningPublicKey(self.public_key.clone())),
                    }),
                    name: self.name.clone(),
                },
            )),
        }
    }
}

/// The Ed25519 public key of `seed`, as Canton reports it.
fn ed25519_public_key(seed: &[u8; 32]) -> SigningPublicKey {
    let mut der = ED25519_SPKI_PREFIX.to_vec();
    der.extend_from_slice(
        &ed25519_dalek::SigningKey::from_bytes(seed)
            .verifying_key()
            .to_bytes(),
    );
    SigningPublicKey {
        format: CryptoKeyFormat::DerX509SubjectPublicKeyInfo as i32,
        public_key: der,
        usage: vec![SigningKeyUsage::Protocol as i32],
        key_spec: SigningKeySpec::EcCurve25519 as i32,
        ..Default::default()
    }
}

struct Vault {
    keys: Vec<VaultKey>,
    key_download: bool,
    next_seed: u8,
    generated: Vec<String>,
    exported: Vec<Fingerprint>,
    topology: Topology,
    authorized: Vec<AuthorizeRequest>,
}

/// The synchronizer's head state: each mapping in force, with its serial.
/// A namespace delegation that DecMan asks the fake to authorize goes in
/// force at once, as on a participant that holds the namespace key.
#[derive(Default)]
struct Topology {
    delegations: Vec<NamespaceDelegation>,
    namespaces: Vec<(i32, DecentralizedNamespaceDefinition)>,
    p2p: Vec<(i32, PartyToParticipant)>,
    party_to_key: Vec<(i32, PartyToKeyMapping)>,
}

/// The context of a head-state row that adds or replaces a mapping.
fn in_force(serial: i32) -> BaseResult {
    BaseResult {
        operation: enums::TopologyChangeOp::AddReplace as i32,
        serial,
        ..Default::default()
    }
}

/// A topology transaction in its versioned envelope, as Canton serializes it.
fn versioned_transaction(transaction: &TopologyTransaction) -> Vec<u8> {
    versioned(transaction.encode_to_vec())
}

fn versioned(data: Vec<u8>) -> Vec<u8> {
    UntypedVersionedMessage {
        version: 30,
        wrapper: Some(untyped_versioned_message::Wrapper::Data(data)),
    }
    .encode_to_vec()
}

/// An unsigned `AddReplace` of `mapping` at `serial`.
fn unsigned(serial: i32, mapping: topology_mapping::Mapping) -> Result<SignedTopologyTransaction> {
    Ok(SignedTopologyTransaction {
        transaction: versioned_transaction(&TopologyTransaction {
            operation: enums::TopologyChangeOp::AddReplace as i32,
            serial: u32::try_from(serial)?,
            mapping: Some(TopologyMapping {
                mapping: Some(mapping),
            }),
        }),
        ..Default::default()
    })
}

/// The namespace of a party id, the part after `::`.
fn party_namespace(party: &str) -> &str {
    party
        .split_once("::")
        .map_or("", |(_, namespace)| namespace)
}

impl Vault {
    fn list_my_keys(&self, request: ListMyKeysRequest) -> ListMyKeysResponse {
        let filters = request.filters.unwrap_or_default();
        ListMyKeysResponse {
            private_keys_metadata: self
                .keys
                .iter()
                .filter(|key| key.fingerprint().as_str().starts_with(&filters.fingerprint))
                .filter(|key| key.name.contains(&filters.name))
                .map(VaultKey::metadata)
                .collect(),
        }
    }

    fn generate_signing_key(
        &mut self,
        request: GenerateSigningKeyRequest,
    ) -> Result<GenerateSigningKeyResponse, Status> {
        // A participant that holds a KMS key makes new keys in KMS, as a
        // KMS-backed Canton does. Otherwise the key goes to the software
        // vault.
        let on_kms = self
            .keys
            .iter()
            .any(|key| key.kms_key_id.as_deref().is_some_and(|id| !id.is_empty()));
        let key = if on_kms {
            VaultKey::kms(&request.name, self.next_seed)
                .map_err(|e| Status::internal(e.to_string()))?
        } else {
            VaultKey::software(&request.name, self.next_seed)
        };
        self.next_seed = self.next_seed.wrapping_add(1);
        self.generated.push(request.name);
        let public_key = key.public_key.clone();
        self.keys.push(key);
        Ok(GenerateSigningKeyResponse {
            public_key: Some(generate_signing_key_response::PublicKey::V30(public_key)),
        })
    }

    fn export_key_pair(
        &mut self,
        request: ExportKeyPairRequest,
    ) -> Result<ExportKeyPairResponse, Status> {
        if !self.key_download {
            return Err(Status::failed_precondition(KEY_DOWNLOAD_OFF));
        }
        let key = self
            .keys
            .iter()
            .find(|key| key.fingerprint().as_str() == request.fingerprint)
            .ok_or_else(|| Status::not_found(format!("no key {}", request.fingerprint)))?;
        let seed = key
            .seed
            .ok_or_else(|| Status::failed_precondition("a KMS key cannot be exported"))?;
        self.exported.push(key.fingerprint());
        // The private key in PKCS#8, followed by the public key, as in the
        // key pair Canton exports.
        let mut key_pair = ED25519_PKCS8_PREFIX.to_vec();
        key_pair.extend_from_slice(&seed);
        key_pair.extend_from_slice(&key.public_key.public_key);
        Ok(ExportKeyPairResponse { key_pair })
    }
}

impl Vault {
    fn list_delegations(
        &self,
        request: ListNamespaceDelegationRequest,
    ) -> ListNamespaceDelegationResponse {
        ListNamespaceDelegationResponse {
            results: self
                .topology
                .delegations
                .iter()
                .filter(|delegation| delegation.namespace.starts_with(&request.filter_namespace))
                .filter(|delegation| {
                    delegation.target_key.as_ref().is_some_and(|key| {
                        compute_fingerprint(key)
                            .as_str()
                            .starts_with(&request.filter_target_key_fingerprint)
                    })
                })
                .map(|delegation| list_namespace_delegation_response::Result {
                    context: Some(in_force(1)),
                    item: Some(list_namespace_delegation_response::result::Item::V30(
                        delegation.clone(),
                    )),
                })
                .collect(),
        }
    }

    fn list_namespaces(
        &self,
        request: ListDecentralizedNamespaceDefinitionRequest,
    ) -> ListDecentralizedNamespaceDefinitionResponse {
        ListDecentralizedNamespaceDefinitionResponse {
            results: self
                .topology
                .namespaces
                .iter()
                .filter(|(_, def)| {
                    def.decentralized_namespace
                        .starts_with(&request.filter_namespace)
                })
                .map(
                    |(serial, def)| list_decentralized_namespace_definition_response::Result {
                        context: Some(in_force(*serial)),
                        item: Some(def.clone()),
                    },
                )
                .collect(),
        }
    }

    fn list_p2p(&self, request: ListPartyToParticipantRequest) -> ListPartyToParticipantResponse {
        ListPartyToParticipantResponse {
            results: self
                .topology
                .p2p
                .iter()
                .filter(|(_, p2p)| p2p.party.starts_with(&request.filter_party))
                .map(|(serial, p2p)| list_party_to_participant_response::Result {
                    context: Some(in_force(*serial)),
                    item: Some(list_party_to_participant_response::result::Item::V30(
                        p2p.clone(),
                    )),
                })
                .collect(),
        }
    }

    fn list_party_to_key(
        &self,
        request: ListPartyToKeyMappingRequest,
    ) -> ListPartyToKeyMappingResponse {
        ListPartyToKeyMappingResponse {
            results: self
                .topology
                .party_to_key
                .iter()
                .filter(|(_, mapping)| mapping.party.starts_with(&request.filter_party))
                .map(
                    |(serial, mapping)| list_party_to_key_mapping_response::Result {
                        context: Some(in_force(*serial)),
                        item: Some(list_party_to_key_mapping_response::result::Item::V30(
                            mapping.clone(),
                        )),
                    },
                )
                .collect(),
        }
    }

    fn list_all(&self, request: ListAllRequest) -> Result<ListAllResponse, Status> {
        let filter = request.filter_namespace.as_str();
        let namespaces = self
            .topology
            .namespaces
            .iter()
            .filter(|(_, def)| def.decentralized_namespace.starts_with(filter))
            .map(|(serial, def)| {
                (
                    *serial,
                    topology_mapping::Mapping::DecentralizedNamespaceDefinition(def.clone()),
                )
            });
        let p2p = self
            .topology
            .p2p
            .iter()
            .filter(|(_, p2p)| party_namespace(&p2p.party).starts_with(filter))
            .map(|(serial, p2p)| {
                (
                    *serial,
                    topology_mapping::Mapping::PartyToParticipant(p2p.clone()),
                )
            });
        let items = namespaces
            .chain(p2p)
            .map(|(serial, mapping)| {
                Ok(topology_transactions::Item {
                    transaction: versioned(unsigned(serial, mapping)?.encode_to_vec()),
                    ..Default::default()
                })
            })
            .collect::<Result<Vec<_>>>()
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(ListAllResponse {
            result: Some(TopologyTransactions { items }),
        })
    }

    fn authorize(&mut self, request: AuthorizeRequest) -> Result<AuthorizeResponse, Status> {
        self.authorized.push(request.clone());
        let Some(authorize_request::Type::Proposal(proposal)) = request.r#type else {
            return Err(Status::unimplemented("the fake authorizes proposals only"));
        };
        let Some(authorize_request::proposal::Mapping::V30(TopologyMapping {
            mapping: Some(mapping),
        })) = proposal.mapping
        else {
            return Err(Status::unimplemented(
                "the fake authorizes v30 mappings only",
            ));
        };
        if let topology_mapping::Mapping::NamespaceDelegation(delegation) = &mapping {
            self.topology.delegations.push(delegation.clone());
        }
        let serial = i32::try_from(proposal.serial.max(1))
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        Ok(AuthorizeResponse {
            transaction: Some(
                unsigned(serial, mapping).map_err(|e| Status::internal(e.to_string()))?,
            ),
        })
    }
}

type Shared = Arc<Mutex<Vault>>;

fn lock(vault: &Shared) -> MutexGuard<'_, Vault> {
    vault.lock().unwrap_or_else(|e| e.into_inner())
}

/// A running fake admin API. The server stops when this is dropped.
pub(crate) struct FakeAdminApi {
    vault: Shared,
    port: u16,
    server: JoinHandle<()>,
}

impl FakeAdminApi {
    /// Serve a vault that holds `keys`, generates software keys, and allows
    /// key download.
    pub async fn start(keys: Vec<VaultKey>) -> Result<Self> {
        let vault = Arc::new(Mutex::new(Vault {
            keys,
            key_download: true,
            next_seed: 100,
            generated: Vec::new(),
            exported: Vec::new(),
            topology: Topology::default(),
            authorized: Vec::new(),
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let router = Server::builder()
            .add_service(VaultService(vault.clone()))
            .add_service(ConnectivityService(vault.clone()))
            .add_service(TopologyReadService(vault.clone()))
            .add_service(TopologyWriteService(vault.clone()))
            .add_service(IdentityService(vault.clone()));
        let server = tokio::spawn(async move {
            if let Err(e) = router
                .serve_with_incoming(TcpIncoming::from(listener))
                .await
            {
                tracing::error!("The fake admin API stopped: {e}");
            }
        });
        Ok(Self {
            vault,
            port,
            server,
        })
    }

    /// Refuse every export, as Canton 3.7 does while key download is off.
    pub fn turn_off_key_download(&self) {
        lock(&self.vault).key_download = false;
    }

    /// The names of the keys the vault generated, in order.
    pub fn generated(&self) -> Vec<String> {
        lock(&self.vault).generated.clone()
    }

    /// The fingerprints of the keys the vault exported, in order.
    pub fn exported(&self) -> Vec<Fingerprint> {
        lock(&self.vault).exported.clone()
    }

    /// Put `def` in force at `serial`.
    pub fn publish_namespace(&self, serial: i32, def: DecentralizedNamespaceDefinition) {
        lock(&self.vault).topology.namespaces.push((serial, def));
    }

    /// Put `mapping` in force at `serial`.
    pub fn publish_p2p(&self, serial: i32, mapping: PartyToParticipant) {
        lock(&self.vault).topology.p2p.push((serial, mapping));
    }

    /// Put the legacy `mapping` in force at `serial`.
    pub fn publish_party_to_key(&self, serial: i32, mapping: PartyToKeyMapping) {
        lock(&self.vault)
            .topology
            .party_to_key
            .push((serial, mapping));
    }

    /// Each `Authorize` request DecMan sent, in order.
    pub fn authorized(&self) -> Vec<AuthorizeRequest> {
        lock(&self.vault).authorized.clone()
    }

    /// A node whose participant is this fake, following `rule` for party keys
    /// outside KMS. `store` turns DecMan's key store on with a key file.
    pub async fn node(&self, rule: SoftwareKeys, store: bool) -> Result<TestNode> {
        let root = tempfile::tempdir()?;
        let mut config = NodeConfig::default().with_root_dir(root.path());
        config.node.participant_id = Some(CantonId::parse(PARTICIPANT_ID)?);
        config.canton.admin_api_host = "127.0.0.1".to_string();
        config.canton.admin_api_port = self.port;
        config.insecure = rule == SoftwareKeys::Allowed;
        config.allow_software_party_keys = rule == SoftwareKeys::Stored;
        if store {
            let path = root.path().join("key-store.key");
            tokio::fs::write(&path, hex::encode([0x5a; 32])).await?;
            config.key_store_key_file = Some(path);
        }
        Ok(TestNode {
            config,
            _root: root,
        })
    }
}

impl Drop for FakeAdminApi {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// A node config and the directory it keeps its files in.
pub(crate) struct TestNode {
    pub config: NodeConfig,
    _root: TempDir,
}

impl TestNode {
    /// Record `key` as this node's Daml key for `party`, as onboarding does
    /// once the party id is known.
    pub async fn record_daml_key(
        &self,
        db: &SqlitePool,
        party: &CantonId,
        key: &VaultKey,
    ) -> Result {
        let namespace = VaultKey::software("namespace", 1);
        db.write_identity(
            party,
            identity_kinds::PEER_PUBLIC_KEYS,
            &self.config.participant_id().to_string(),
            &encode_keys_payload(&namespace.public_key, &key.public_key),
        )
        .await
    }
}

/// Answer one unary call with `handler`.
async fn unary<Req, Resp>(
    request: http::Request<Body>,
    handler: impl FnMut(Req) -> Result<Resp, Status> + Send + 'static,
) -> http::Response<Body>
where
    Req: prost::Message + Default + Send + 'static,
    Resp: prost::Message + Send + 'static,
{
    Grpc::new(tonic_prost::ProstCodec::<Resp, Req>::default())
        .unary(Handler(handler), request)
        .await
}

struct Handler<F>(F);

impl<F, Req, Resp> Service<Request<Req>> for Handler<F>
where
    F: FnMut(Req) -> Result<Resp, Status>,
{
    type Response = Response<Resp>;
    type Error = Status;
    type Future = std::future::Ready<Result<Response<Resp>, Status>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Status>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Req>) -> Self::Future {
        std::future::ready((self.0)(request.into_inner()).map(Response::new))
    }
}

async fn dispatch(vault: Shared, request: http::Request<Body>) -> http::Response<Body> {
    let path = request.uri().path().to_string();
    match path.rsplit_once('/').map(|(_, method)| method) {
        Some("ListMyKeys") => {
            unary(request, move |r: ListMyKeysRequest| {
                Ok(lock(&vault).list_my_keys(r))
            })
            .await
        }
        Some("GenerateSigningKey") => {
            unary(request, move |r: GenerateSigningKeyRequest| {
                lock(&vault).generate_signing_key(r)
            })
            .await
        }
        Some("ExportKeyPair") => {
            unary(request, move |r: ExportKeyPairRequest| {
                lock(&vault).export_key_pair(r)
            })
            .await
        }
        Some("ListNamespaceDelegation") => {
            unary(request, move |r: ListNamespaceDelegationRequest| {
                Ok(lock(&vault).list_delegations(r))
            })
            .await
        }
        Some("ListDecentralizedNamespaceDefinition") => {
            unary(
                request,
                move |r: ListDecentralizedNamespaceDefinitionRequest| {
                    Ok(lock(&vault).list_namespaces(r))
                },
            )
            .await
        }
        Some("ListPartyToParticipant") => {
            unary(request, move |r: ListPartyToParticipantRequest| {
                Ok(lock(&vault).list_p2p(r))
            })
            .await
        }
        Some("ListPartyToKeyMapping") => {
            unary(request, move |r: ListPartyToKeyMappingRequest| {
                Ok(lock(&vault).list_party_to_key(r))
            })
            .await
        }
        Some("ListAll") => unary(request, move |r: ListAllRequest| lock(&vault).list_all(r)).await,
        Some("Authorize") => {
            unary(request, move |r: AuthorizeRequest| {
                lock(&vault).authorize(r)
            })
            .await
        }
        Some("SignTransactions") => {
            unary(request, |r: SignTransactionsRequest| {
                Ok(SignTransactionsResponse {
                    transactions: r.transactions,
                })
            })
            .await
        }
        Some("GetId") => {
            unary(request, |_: GetIdRequest| {
                Ok(GetIdResponse {
                    initialized: true,
                    unique_identifier: PARTICIPANT_ID.to_string(),
                })
            })
            .await
        }
        Some("GetSynchronizerId") => {
            unary(request, |_: GetSynchronizerIdRequest| {
                Ok(GetSynchronizerIdResponse {
                    synchronizer_id: String::new(),
                    physical_synchronizer_id: SYNCHRONIZER_ID.to_string(),
                })
            })
            .await
        }
        _ => Status::unimplemented(path).into_http(),
    }
}

/// A gRPC service of the fake under Canton's service name. Every service
/// shares the one dispatcher, which routes on the method name.
macro_rules! fake_service {
    ($service:ident, $name:literal) => {
        #[derive(Clone)]
        struct $service(Shared);

        impl NamedService for $service {
            const NAME: &'static str = $name;
        }

        impl Service<http::Request<Body>> for $service {
            type Response = http::Response<Body>;
            type Error = Infallible;
            type Future = BoxFuture<Self::Response, Infallible>;

            fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
                Poll::Ready(Ok(()))
            }

            fn call(&mut self, request: http::Request<Body>) -> Self::Future {
                let vault = self.0.clone();
                Box::pin(async move { Ok(dispatch(vault, request).await) })
            }
        }
    };
}

fake_service!(
    VaultService,
    "com.digitalasset.canton.crypto.admin.v30.VaultService"
);
fake_service!(
    ConnectivityService,
    "com.digitalasset.canton.admin.participant.v30.SynchronizerConnectivityService"
);
fake_service!(
    TopologyReadService,
    "com.digitalasset.canton.topology.admin.v30.TopologyManagerReadService"
);
fake_service!(
    TopologyWriteService,
    "com.digitalasset.canton.topology.admin.v30.TopologyManagerWriteService"
);
fake_service!(
    IdentityService,
    "com.digitalasset.canton.topology.admin.v30.IdentityInitializationService"
);
