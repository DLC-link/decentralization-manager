//! Display helpers: names, the merged peer view, mesh error text and picker
//! labels.

use std::collections::HashMap;

use common::types::{AuditLogEntry, ConnectionStatus, DecentralizedParty, ParticipantStatus};
use common::types::{PendingInvitation, VettedPackageInfo, WorkflowInfo, WorkflowRun};
use serde_json::Value;

use super::types::{MeshErrorResponse, NodeConfigResponse, PeerConfig};
use crate::composer::{PickerOption, PickerSource};

/// Human-readable party name: the prefix of the Canton party id (the segment
/// before `::`), which the [`CantonId`] type already splits out.
pub fn party_name(party: &DecentralizedParty) -> &str {
    party.party_id.prefix.as_str()
}

/// Sort vetted packages by name, then version — matching the web frontend.
pub(super) fn sort_vetted(packages: &mut [VettedPackageInfo]) {
    packages.sort_by(|a, b| {
        a.package_name
            .cmp(&b.package_name)
            .then_with(|| a.package_version.cmp(&b.package_version))
    });
}

/// A peer merged from `/network-config` and live `/participants-status`, ready
/// for display — the same information the web frontend's network panel shows.
#[derive(Clone, Debug)]
pub struct PeerView {
    pub participant_id: String,
    pub name: String,
    pub address: String,
    pub port: u16,
    /// Live connectivity status, or `None` when no status was reported for this
    /// peer (rendered as "Unknown").
    pub status: Option<ConnectionStatus>,
    pub latency_ms: Option<u64>,
    pub version: Option<String>,
    pub workflow: Option<String>,
    pub is_self: bool,
}

/// Display name for a peer: its configured name, else the id prefix.
fn display_name(name: &str, participant_id: &str) -> String {
    if name.is_empty() {
        participant_id
            .split("::")
            .next()
            .unwrap_or(participant_id)
            .to_owned()
    } else {
        name.to_owned()
    }
}

/// Format a peer's active workflow as `Kind (step)` (or just `Kind`).
fn format_workflow(workflow: &WorkflowInfo) -> String {
    if workflow.step.is_empty() {
        workflow.kind.to_string()
    } else {
        format!(
            "{kind} ({step})",
            kind = workflow.kind,
            step = workflow.step
        )
    }
}

/// Merge this node and the configured peers with their live statuses, putting
/// this node first. If this node is not in the peer list, it is synthesized
/// from `/node-config` so it is always shown — exactly like the web frontend.
pub(super) fn merge_peers(
    node: &NodeConfigResponse,
    peers: &[PeerConfig],
    statuses: &[ParticipantStatus],
    self_latency_ms: Option<u64>,
) -> Vec<PeerView> {
    let by_id: HashMap<&str, &ParticipantStatus> =
        statuses.iter().map(|s| (s.id.as_str(), s)).collect();
    let self_id = node.node.participant_id.as_str();
    let mut self_seen = false;

    let mut views: Vec<PeerView> = peers
        .iter()
        .map(|peer| {
            let live = by_id.get(peer.participant_id.as_str());
            let is_self = peer.participant_id == self_id;
            self_seen |= is_self;
            let status = if is_self {
                Some(ConnectionStatus::CurrentNode)
            } else {
                live.map(|s| s.status)
            };
            PeerView {
                participant_id: peer.participant_id.clone(),
                name: display_name(&peer.name, &peer.participant_id),
                address: peer.address.clone(),
                port: peer.port,
                status,
                // Peers report a measured round-trip; self uses our own
                // API round-trip instead (a peer never probes itself).
                latency_ms: if is_self {
                    self_latency_ms
                } else {
                    live.and_then(|s| s.latency_ms)
                },
                version: live
                    .and_then(|s| s.version.clone())
                    .or_else(|| if is_self { node.version.clone() } else { None }),
                workflow: live.and_then(|s| s.workflow.as_ref()).map(format_workflow),
                is_self,
            }
        })
        .collect();

    if !self_seen {
        let address = node
            .node
            .public_address
            .clone()
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| node.node.listen_address.clone());
        views.push(PeerView {
            participant_id: self_id.to_owned(),
            name: display_name("", self_id),
            address,
            port: node.node.port,
            status: Some(ConnectionStatus::CurrentNode),
            latency_ms: self_latency_ms,
            version: node.version.clone(),
            workflow: None,
            is_self: true,
        });
    }

    // This node first, then the rest in configured order (stable sort).
    views.sort_by_key(|peer| !peer.is_self);
    views
}

/// Human-readable name for a workflow run: the target party prefix when known,
/// otherwise the instance name.
pub fn run_name(run: &WorkflowRun) -> &str {
    match run.prefix.as_deref() {
        Some(prefix) if !prefix.is_empty() => prefix,
        _ => run.instance_name.as_str(),
    }
}

/// Human-readable name for a pending invitation: the target party prefix, else
/// the coordinator.
pub fn invitation_name(invitation: &PendingInvitation) -> String {
    match invitation.prefix.as_deref() {
        Some(prefix) if !prefix.is_empty() => prefix.to_owned(),
        _ => invitation
            .coordinator_name
            .clone()
            .map(|name| format!("from {name}"))
            .unwrap_or_else(|| "from peer".to_owned()),
    }
}

/// Human action label for an audit entry: the summary when present, else the
/// event type.
pub fn audit_action(entry: &AuditLogEntry) -> &str {
    if entry.action_summary.is_empty() {
        &entry.event_type
    } else {
        &entry.action_summary
    }
}

/// The remediation hints for a `422` onboarding body, or `None` if the body is
/// not a mesh error.
pub fn mesh_error_hint(body: &str) -> Option<String> {
    serde_json::from_str::<MeshErrorResponse>(body)
        .ok()
        .map(|mesh| format_mesh_error(&mesh))
}

/// Render a mesh pre-flight failure as actionable, multi-line remediation
/// guidance, mirroring the web frontend's onboarding error panel.
fn format_mesh_error(mesh: &MeshErrorResponse) -> String {
    let mut out = if mesh.error.is_empty() {
        "Peers are not fully meshed.".to_owned()
    } else {
        mesh.error.clone()
    };
    if !mesh.missing_edges.is_empty() {
        out.push_str("\n\nMissing connections:");
        for edge in &mesh.missing_edges {
            let hint = match edge.kind.as_deref() {
                Some("unreachable_from_coordinator") => {
                    format!("coordinator can't reach {to}", to = short_id(&edge.to))
                }
                _ => format!(
                    "on {from}, add {to} as a peer",
                    from = short_id(&edge.from),
                    to = short_id(&edge.to)
                ),
            };
            out.push_str(&format!("\n • {hint}"));
        }
    }
    out
}

/// The human-readable prefix of a Canton id (the segment before `::`).
fn short_id(id: &str) -> &str {
    id.split("::").next().unwrap_or(id)
}

/// The endpoint path and response envelope key for a picker source.
pub(super) fn source_endpoint(source: PickerSource) -> (&'static str, &'static str) {
    match source {
        PickerSource::Instruments => ("/instruments", "instruments"),
        PickerSource::ProviderServices => ("/services/provider", "services"),
        PickerSource::UserServices => ("/services/user", "services"),
        PickerSource::RegistrarServices => ("/services/registrar", "services"),
        PickerSource::CredentialOffers => ("/credential-offers", "credential_offers"),
        PickerSource::MintRequests => ("/governance/mint-requests", "mint_requests"),
        PickerSource::BurnRequests => ("/governance/burn-requests", "burn_requests"),
        PickerSource::TransferInstructions => {
            ("/governance/transfer-instructions", "transfer_instructions")
        }
        PickerSource::TransferFactories => ("/transfer-factories", "transfer_factories"),
    }
}

/// Read a string field off a JSON contract object (empty when absent).
fn str_field(item: &Value, key: &str) -> String {
    item.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A short, always-non-empty stand-in label for a contract with no better
/// summary: the first characters of its id.
fn short_cid(cid: &str) -> String {
    let head: String = cid.chars().take(14).collect();
    if cid.chars().count() > 14 {
        format!("{head}…")
    } else {
        head
    }
}

/// A one-line human summary for a fetched contract, formatted per source to
/// mirror the web frontend's dropdown labels.
fn picker_label(source: PickerSource, item: &Value) -> String {
    let f = |key: &str| str_field(item, key);
    let p = |key: &str| short_id(&f(key)).to_owned();
    let label = match source {
        PickerSource::Instruments => format!("{} · {}", f("instrument_id"), p("instrument_admin")),
        PickerSource::ProviderServices => format!("provider {}", p("provider")),
        PickerSource::UserServices => format!("user {}", p("user")),
        PickerSource::RegistrarServices => format!("registrar {}", p("registrar")),
        PickerSource::CredentialOffers => format!("{} → {}", f("credential_id"), p("holder")),
        PickerSource::MintRequests | PickerSource::BurnRequests => {
            format!("{} {} → {}", f("amount"), f("instrument_id"), p("holder"))
        }
        PickerSource::TransferInstructions => format!(
            "{} {} {}→{}",
            f("amount"),
            f("instrument_id"),
            p("sender"),
            p("receiver")
        ),
        PickerSource::TransferFactories => format!("admin {}", p("expected_admin")),
    };
    label.trim().to_owned()
}

/// Turn a JSON contract object into a [`PickerOption`], or `None` when it has no
/// contract id (e.g. the synthetic shared-instrument transfer-factory rows).
pub(super) fn picker_option(source: PickerSource, item: &Value) -> Option<PickerOption> {
    let value = str_field(item, "contract_id");
    if value.is_empty() {
        return None;
    }
    let label = picker_label(source, item);
    let label = if label.chars().any(char::is_alphanumeric) {
        label
    } else {
        short_cid(&value)
    };
    Some(PickerOption { value, label })
}

#[cfg(test)]
mod tests {
    use common::canton_id::CantonId;
    use serde_json::json;

    use super::*;
    use crate::api::fixtures::NS;
    use crate::api::types::{MeshEdge, NodeIdentity};

    fn party(prefix: &str) -> DecentralizedParty {
        DecentralizedParty {
            party_id: CantonId::parse(&format!("{prefix}::{NS}")).unwrap(),
            threshold: 1,
            confirmation_threshold: None,
            owners: Vec::new(),
            my_owner_key: None,
            participants: Vec::new(),
            non_hosting_owners: Vec::new(),
            contracts: Vec::new(),
            local_metadata: None,
        }
    }

    #[test]
    fn party_name_is_the_canton_id_prefix() {
        assert_eq!(party_name(&party("cbtc-network")), "cbtc-network");
    }

    #[test]
    fn short_id_takes_the_prefix() {
        assert_eq!(short_id("alpha::1220deadbeef"), "alpha");
        assert_eq!(short_id("nocolon"), "nocolon");
    }

    #[test]
    fn format_mesh_error_renders_remediation_hints() {
        // Arrange — one coordinator-reachability gap and one peer mesh hole.
        let mesh = MeshErrorResponse {
            error: "Could not verify a full peer mesh.".to_owned(),
            missing_edges: vec![
                MeshEdge {
                    from: "coord::1220ab".to_owned(),
                    to: "peerB::1220cd".to_owned(),
                    kind: Some("unreachable_from_coordinator".to_owned()),
                },
                MeshEdge {
                    from: "peerA::1220ef".to_owned(),
                    to: "peerB::1220cd".to_owned(),
                    kind: Some("mesh_hole".to_owned()),
                },
            ],
        };

        // Act
        let out = format_mesh_error(&mesh);

        // Assert — the server message plus per-edge, short-id remediation.
        assert!(out.contains("Could not verify a full peer mesh."));
        assert!(out.contains("coordinator can't reach peerB"));
        assert!(out.contains("on peerA, add peerB as a peer"));
    }

    #[test]
    fn format_mesh_error_falls_back_without_an_error_message() {
        let mesh = MeshErrorResponse {
            error: String::new(),
            missing_edges: Vec::new(),
        };
        assert_eq!(format_mesh_error(&mesh), "Peers are not fully meshed.");
    }

    fn node(participant_id: &str) -> NodeConfigResponse {
        NodeConfigResponse {
            node: NodeIdentity {
                participant_id: participant_id.to_owned(),
                public_address: Some("node.local".to_owned()),
                listen_address: "0.0.0.0".to_owned(),
                port: 9000,
            },
            version: Some("0.9.1".to_owned()),
        }
    }

    fn status(id: &str, workflow: Option<WorkflowInfo>) -> ParticipantStatus {
        ParticipantStatus {
            id: id.to_owned(),
            status: ConnectionStatus::Connected,
            latency_ms: Some(12),
            version: Some("1.2.3".to_owned()),
            build_version: Some("v1.2.3".to_owned()),
            wire_version: None,
            workflow,
        }
    }

    #[test]
    fn merge_peers_puts_self_first_and_maps_status() {
        // Arrange — self is present in the peer list.
        let peers = [
            PeerConfig {
                participant_id: "peer1::1220".to_owned(),
                name: "alpha".to_owned(),
                address: "10.0.0.1".to_owned(),
                port: 9001,
            },
            PeerConfig {
                participant_id: "self::1220".to_owned(),
                name: "me".to_owned(),
                address: "127.0.0.1".to_owned(),
                port: 9000,
            },
        ];
        let statuses = [status(
            "peer1::1220",
            Some(WorkflowInfo {
                kind: common::types::WorkflowKind::Onboarding,
                role: common::types::WorkflowRole::Peer,
                step: "Sign".to_owned(),
                step_index: 0,
                step_total: 0,
            }),
        )];

        // Act
        let views = merge_peers(&node("self::1220"), &peers, &statuses, Some(7));

        // Assert — self first, peer status/latency/workflow mapped.
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].name, "me");
        assert!(views[0].is_self);
        assert_eq!(views[0].status, Some(ConnectionStatus::CurrentNode));
        // Self shows our own API round-trip latency, not a peer probe.
        assert_eq!(views[0].latency_ms, Some(7));
        assert_eq!(views[1].name, "alpha");
        assert_eq!(views[1].status, Some(ConnectionStatus::Connected));
        assert_eq!(views[1].latency_ms, Some(12));
        assert_eq!(views[1].workflow.as_deref(), Some("Onboarding (Sign)"));
    }

    #[test]
    fn merge_peers_synthesizes_self_when_absent() {
        // Arrange — this node is NOT in the peer list, and a peer has no status.
        let peers = [PeerConfig {
            participant_id: "ghost::1220".to_owned(),
            name: "ghost".to_owned(),
            address: "10.0.0.9".to_owned(),
            port: 9009,
        }];

        // Act
        let views = merge_peers(&node("self::1220"), &peers, &[], Some(9));

        // Assert — self synthesized and listed first; missing status → None (Unknown).
        assert_eq!(views.len(), 2);
        assert!(views[0].is_self);
        assert_eq!(views[0].status, Some(ConnectionStatus::CurrentNode));
        assert_eq!(views[0].version.as_deref(), Some("0.9.1"));
        assert_eq!(views[0].latency_ms, Some(9));
        assert!(!views[1].is_self);
        assert_eq!(views[1].name, "ghost");
        assert_eq!(views[1].status, None);
    }

    #[test]
    fn sort_vetted_orders_by_name_then_version() {
        // Arrange
        let mut packages = vec![
            VettedPackageInfo {
                package_id: "c".to_owned(),
                package_name: "splice".to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            VettedPackageInfo {
                package_id: "a".to_owned(),
                package_name: "daml-stdlib".to_owned(),
                package_version: "2.0.0".to_owned(),
            },
            VettedPackageInfo {
                package_id: "b".to_owned(),
                package_name: "daml-stdlib".to_owned(),
                package_version: "1.0.0".to_owned(),
            },
        ];

        // Act
        sort_vetted(&mut packages);

        // Assert
        let order: Vec<&str> = packages.iter().map(|p| p.package_id.as_str()).collect();
        assert_eq!(order, ["b", "a", "c"]);
    }

    #[test]
    fn source_endpoint_maps_paths_and_envelope_keys() {
        assert_eq!(
            source_endpoint(PickerSource::MintRequests),
            ("/governance/mint-requests", "mint_requests")
        );
        assert_eq!(
            source_endpoint(PickerSource::ProviderServices),
            ("/services/provider", "services")
        );
    }

    #[test]
    fn picker_option_skips_empty_cid_and_labels_by_source() {
        // A contract with no id (e.g. a synthetic transfer-factory row) is dropped.
        assert!(
            picker_option(
                PickerSource::Instruments,
                &json!({ "instrument_id": "CBTC" })
            )
            .is_none()
        );

        // Mint request: amount, instrument, holder prefix.
        let mint = picker_option(
            PickerSource::MintRequests,
            &json!({
                "contract_id": "00m", "amount": "100", "instrument_id": "CBTC",
                "holder": "alice::1220",
            }),
        )
        .expect("mint option");
        assert_eq!(mint.label, "100 CBTC → alice");
    }

    #[test]
    fn picker_option_falls_back_to_short_cid_without_label_fields() {
        let option = picker_option(
            PickerSource::Instruments,
            &json!({ "contract_id": "00abcdef0123456789" }),
        )
        .expect("instrument option");
        // No instrument_id / admin → the label degrades to a truncated cid.
        assert_eq!(option.label, "00abcdef012345…");
    }
}
