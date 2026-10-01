use std::{
    collections::{BTreeSet, HashMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{
        DarDescription, GetDarRequest, GetPackageReferencesRequest, ListPackagesRequest,
        package_service_client::PackageServiceClient,
    },
    protocol::v30::{enums::TopologyChangeOp, vetted_packages::VettedPackage},
    topology::admin::v30::{
        BaseQuery, ListVettedPackagesRequest, list_vetted_packages_response,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use chrono::{DateTime, Utc};
use prost_types::Timestamp;
use serde_json::Value;
use tonic::transport::Channel;

use common::canton_id::CantonId;

use crate::{
    config::NodeConfig,
    utils,
    workflow::{contracts::DarFile, topology},
};

use super::{
    queries::compare_versions,
    types::{ExpectedPackageVersion, VettedPackageInfo},
};

/// Derive the stable package-name prefix from a package reference by
/// stripping the leading `#` and any trailing version segments, e.g.
/// `#governance-core-v1-rc1` → `governance-core`.
pub(crate) fn package_name_prefix(package_ref: &str) -> String {
    let name = package_ref.strip_prefix('#').unwrap_or(package_ref);
    let mut segments: Vec<&str> = name.split('-').collect();
    while segments.len() > 1 {
        let is_version = segments
            .last()
            .and_then(|s| s.strip_prefix("rc").or_else(|| s.strip_prefix('v')))
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()));
        if !is_version {
            break;
        }
        segments.pop();
    }
    segments.join("-")
}

/// Names from the participant's package inventory that belong to the package
/// family identified by `prefix` — any version, including renamed historical
/// uploads like `governance-core-v0-rc3`.
pub(crate) fn matching_names<'a>(package_names: &'a [String], prefix: &str) -> BTreeSet<&'a str> {
    package_names
        .iter()
        .filter(|name| package_name_prefix(name) == prefix)
        .map(String::as_str)
        .collect()
}

/// Package names sharing `prefix`, sorted newest-first by their version tail
/// (`governance-core-v1-rc1` before `governance-core-v0-rc4`). The first
/// element is the newest version present on the participant.
pub(crate) fn newest_matching_names(package_names: &[String], prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = matching_names(package_names, prefix)
        .into_iter()
        .map(str::to_string)
        .collect();
    names.sort_by(|a, b| {
        compare_versions(&version_tail(b, prefix), &version_tail(a, prefix)).then_with(|| b.cmp(a))
    });
    names
}

/// The version portion of `name` after the `prefix`, with `v`/`rc` markers
/// stripped and segments dot-joined so `compare_versions` orders them
/// numerically, e.g. `governance-core-v1-rc1` → `1.1`.
fn version_tail(name: &str, prefix: &str) -> String {
    name.strip_prefix(prefix)
        .unwrap_or(name)
        .trim_start_matches('-')
        .split('-')
        .map(|seg| seg.trim_start_matches("rc").trim_start_matches('v'))
        .collect::<Vec<_>>()
        .join(".")
}

/// Load the names of all packages uploaded to the participant from the Admin
/// API's PackageService.
pub(crate) async fn fetch_package_names(config: &NodeConfig) -> Result<Vec<String>> {
    let mut client = PackageServiceClient::new(
        config
            .admin_channel()
            .await
            .context("Failed to connect to participant Admin API")?,
    );
    let response = client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            limit: 0,
            filter_name: String::new(),
        }))
        .await
        .context("Failed to list participant packages")?
        .into_inner();
    Ok(response
        .package_descriptions
        .into_iter()
        .map(|p| p.name)
        .collect())
}

/// Load `(package_id → name)` from the participant's Admin PackageService.
/// Used to resolve a contract's concrete package id back to a `#name` ref.
pub(crate) async fn fetch_package_id_to_name(
    config: &NodeConfig,
) -> Result<HashMap<String, String>> {
    let mut client = PackageServiceClient::new(
        config
            .admin_channel()
            .await
            .context("Failed to connect to participant Admin API")?,
    );
    let response = client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            limit: 0,
            filter_name: String::new(),
        }))
        .await
        .context("Failed to list participant packages")?
        .into_inner();
    Ok(response
        .package_descriptions
        .into_iter()
        .map(|p| (p.package_id, p.name))
        .collect())
}

/// Packages this participant has vetted and that are in effect right now,
/// with name and version.
///
/// The topology entries carry only package ids, so name/version are joined in
/// from the Admin PackageService. A vetted package can be missing there — a
/// restore from backup keeps the vetting but not the DAR — and then name and
/// version stay empty: vetting is topology state, not local package state.
///
/// Queries the synchronizer store: the DAR upload path registers vetting
/// directly on the synchronizer, so on a live node the Authorized store holds
/// an empty or stale copy (#376). Only `Replace` mappings are requested — at
/// head state a `Remove` means "no longer vetted", and counting its package
/// list would report a fully unvetted participant as vetted. Entries outside
/// their validity window are dropped too: Splice schedules upgrades by vetting
/// with a future `valid_from_inclusive`, which Canton rejects until that time
/// arrives.
///
/// Deliberately on the admin channel: the Ledger API has a paginated
/// `ListVettedPackages`, but it needs a bearer token and tokens here are
/// per-party — a participant-level endpoint has no party to borrow one from.
pub(crate) async fn fetch_vetted_packages(config: &NodeConfig) -> Result<Vec<VettedPackageInfo>> {
    let ids = fetch_vetted_package_ids(config, config.participant_id()).await?;
    let descriptions = fetch_package_descriptions(config).await?;

    Ok(ids
        .into_iter()
        .map(|package_id| {
            let (name, version) = descriptions.get(&package_id).cloned().unwrap_or_default();
            VettedPackageInfo {
                package_id,
                package_name: name,
                package_version: version,
            }
        })
        .collect())
}

/// The package ids `participant_id` has vetted right now, in topology order
/// and deduplicated.
///
/// The synchronizer replicates every participant's `VettedPackages` mapping to
/// every member, so this reads a PEER's vetting from the local participant's
/// own topology store. No peer is contacted.
///
/// # Errors
/// Returns an error when the synchronizer id cannot be resolved or the
/// topology read fails.
pub(crate) async fn fetch_vetted_package_ids(
    config: &NodeConfig,
    participant_id: &CantonId,
) -> Result<Vec<String>> {
    TopologyReader::connect(config)
        .await?
        .vetted_package_ids(participant_id)
        .await
}

/// The Splice package that each field of AmuletRules' `packageConfig` pins.
const SPLICE_PACKAGE_CONFIG: [(&str, &str); 6] = [
    ("amulet", "splice-amulet"),
    ("amuletNameService", "splice-amulet-name-service"),
    ("dsoGovernance", "splice-dso-governance"),
    ("validatorLifecycle", "splice-validator-lifecycle"),
    ("wallet", "splice-wallet"),
    ("walletPayments", "splice-wallet-payments"),
];

/// The Splice package versions the DSO has in effect at `now`, from the
/// DSO API's `/dso` response.
///
/// AmuletRules holds a config schedule: an initial value and future values,
/// each with the time it takes effect. The one in effect is the latest future
/// value whose time has passed, else the initial value.
///
/// # Errors
/// Returns an error when the response has no config schedule or a scheduled
/// time does not parse.
pub(crate) fn expected_splice_versions(
    dso: &Value,
    now: DateTime<Utc>,
) -> Result<Vec<ExpectedPackageVersion>> {
    let schedule = dso
        .pointer("/amulet_rules/contract/payload/configSchedule")
        .context("DSO API response has no AmuletRules config schedule")?;
    let mut config = schedule
        .get("initialValue")
        .context("AmuletRules config schedule has no initial value")?;
    let mut effective_since: Option<DateTime<Utc>> = None;
    for entry in schedule
        .get("futureValues")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (time, value) = match entry {
            Value::Array(pair) if pair.len() == 2 => (&pair[0], &pair[1]),
            Value::Object(tuple) => match (tuple.get("_1"), tuple.get("_2")) {
                (Some(time), Some(value)) => (time, value),
                _ => continue,
            },
            _ => continue,
        };
        let time = time
            .as_str()
            .context("AmuletRules scheduled time is not a string")?;
        let time = DateTime::parse_from_rfc3339(time)
            .with_context(|| format!("AmuletRules scheduled time {time} does not parse"))?
            .with_timezone(&Utc);
        if time <= now && effective_since.is_none_or(|since| time > since) {
            config = value;
            effective_since = Some(time);
        }
    }
    let package_config = config
        .get("packageConfig")
        .context("AmuletRules config has no packageConfig")?;
    Ok(SPLICE_PACKAGE_CONFIG
        .iter()
        .filter_map(|(field, package_name)| {
            package_config
                .get(*field)
                .and_then(Value::as_str)
                .map(|version| ExpectedPackageVersion {
                    package_name: (*package_name).to_string(),
                    version: version.to_string(),
                })
        })
        .collect())
}

/// The DAR on this participant that holds `package_id`, ready to distribute.
///
/// A package id can be a DAR's main package or one of its dependencies, so
/// the DAR is found through the package's references. `Ok(None)` means no DAR
/// on this participant holds the package: its vetting can outlive the DAR.
///
/// # Errors
/// Returns an error when the Admin API cannot be reached or a read fails.
pub(crate) async fn fetch_dar_for_package(
    config: &NodeConfig,
    package_id: &str,
) -> Result<Option<DarFile>> {
    let mut client = PackageServiceClient::new(
        config
            .admin_channel()
            .await
            .context("Failed to connect to participant Admin API")?,
    )
    .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);
    let references = client
        .get_package_references(tonic::Request::new(GetPackageReferencesRequest {
            package_id: package_id.to_string(),
        }))
        .await
        .with_context(|| format!("Failed to read the DARs that hold package {package_id}"))?
        .into_inner();
    let Some(dar) = pick_dar(package_id, &references.dars) else {
        return Ok(None);
    };
    let main = dar.main.clone();
    let response = client
        .get_dar(tonic::Request::new(GetDarRequest {
            main_package_id: main.clone(),
        }))
        .await
        .with_context(|| format!("Failed to read DAR {main}"))?
        .into_inner();
    Ok(Some(DarFile {
        filename: dar_filename(dar),
        data: STANDARD.encode(response.payload),
    }))
}

/// The DAR to send for `package_id`: the one it is the main package of, if
/// any, else the first that depends on it. The main-package DAR is the one an
/// operator uploaded to get this package.
fn pick_dar<'a>(package_id: &str, dars: &'a [DarDescription]) -> Option<&'a DarDescription> {
    dars.iter()
        .find(|d| d.main == package_id)
        .or_else(|| dars.first())
}

/// A file name for a DAR, as an upload would have named it.
fn dar_filename(dar: &DarDescription) -> String {
    match (dar.name.is_empty(), dar.version.is_empty()) {
        (false, false) => format!("{}-{}.dar", dar.name, dar.version),
        (false, true) => format!("{}.dar", dar.name),
        _ => format!("{}.dar", dar.main),
    }
}

/// One connected topology reader, reused across several reads.
///
/// Reading a whole peer set through [`fetch_vetted_package_ids`] would resolve
/// the synchronizer id and open an admin channel once per peer. This holds both
/// so a caller pays that cost once.
pub(crate) struct TopologyReader {
    client: TopologyManagerReadServiceClient<Channel>,
    synchronizer_id: String,
}

impl TopologyReader {
    /// Resolve the synchronizer and connect to the participant's Admin API.
    ///
    /// # Errors
    /// Returns an error when the synchronizer id cannot be resolved or the
    /// channel cannot be opened.
    pub(crate) async fn connect(config: &NodeConfig) -> Result<Self> {
        let synchronizer_id = utils::get_synchronizer_id(config).await?;
        let channel = config
            .admin_channel()
            .await
            .context("Failed to connect to participant Admin API")?;
        Ok(Self {
            client: TopologyManagerReadServiceClient::new(channel)
                .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE),
            synchronizer_id,
        })
    }

    /// The package ids `participant_id` has vetted right now.
    ///
    /// # Errors
    /// Returns an error when the topology read fails.
    pub(crate) async fn vetted_package_ids(
        &mut self,
        participant_id: &CantonId,
    ) -> Result<Vec<String>> {
        let wanted = participant_id.to_string();
        let response = self
            .client
            .list_vetted_packages(tonic::Request::new(ListVettedPackagesRequest {
                base_query: Some(BaseQuery {
                    operation: TopologyChangeOp::AddReplace as i32,
                    ..topology::head_state_query(&self.synchronizer_id)
                }),
                filter_participant: wanted.clone(),
            }))
            .await
            .with_context(|| format!("Failed to list vetted packages of {wanted}"))?
            .into_inner();

        Ok(vetted_ids_of(response.results, &wanted, &now_timestamp()))
    }
}

/// The valid, deduplicated package ids that `wanted` itself has vetted.
///
/// Canton splits `filter_participant` on `::` and matches each half as a LIKE
/// prefix, so a request for `participant::1220ab` also returns
/// `participant-2::1220abcd`. Every result whose `participant_uid` is not
/// exactly `wanted` is dropped here. Without that check a node would read a
/// neighbour's vetting as its peer's.
fn vetted_ids_of(
    results: Vec<list_vetted_packages_response::Result>,
    wanted: &str,
    now: &Timestamp,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for result in results {
        let Some(item) = result.item else { continue };
        if item.participant_uid != wanted {
            continue;
        }
        for package in item.packages {
            if package_valid_at(&package, now) && seen.insert(package.package_id.clone()) {
                ids.push(package.package_id);
            }
        }
    }
    ids
}

/// The current wall-clock time as a proto timestamp, for validity checks.
fn now_timestamp() -> Timestamp {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    Timestamp {
        seconds: i64::try_from(now.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(now.subsec_nanos()).unwrap_or(0),
    }
}

/// Whether a vetting entry is in effect at `now`: `valid_from_inclusive` has
/// passed (or is unset) and `valid_until_exclusive` has not (or is unset).
fn package_valid_at(package: &VettedPackage, now: &Timestamp) -> bool {
    let le = |a: &Timestamp, b: &Timestamp| (a.seconds, a.nanos) <= (b.seconds, b.nanos);
    package
        .valid_from_inclusive
        .as_ref()
        .is_none_or(|from| le(from, now))
        && package
            .valid_until_exclusive
            .as_ref()
            .is_none_or(|until| !le(until, now))
}

/// Load `(package_id → (name, version))` from the Admin PackageService.
async fn fetch_package_descriptions(
    config: &NodeConfig,
) -> Result<HashMap<String, (String, String)>> {
    let mut client = PackageServiceClient::new(
        config
            .admin_channel()
            .await
            .context("Failed to connect to participant Admin API")?,
    );
    let response = client
        .list_packages(tonic::Request::new(ListPackagesRequest {
            limit: 0,
            filter_name: String::new(),
        }))
        .await
        .context("Failed to list participant packages")?
        .into_inner();
    Ok(response
        .package_descriptions
        .into_iter()
        .map(|p| (p.package_id, (p.name, p.version)))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dar(main: &str, name: &str, version: &str) -> DarDescription {
        DarDescription {
            main: main.to_string(),
            name: name.to_string(),
            version: version.to_string(),
            description: String::new(),
        }
    }

    fn dso_with_schedule(future_values: Value) -> Value {
        serde_json::json!({
            "amulet_rules": { "contract": { "payload": { "configSchedule": {
                "initialValue": { "packageConfig": {
                    "amulet": "0.1.22",
                    "amuletNameService": "0.1.23",
                    "dsoGovernance": "0.1.28",
                    "validatorLifecycle": "0.1.9",
                    "wallet": "0.1.23",
                    "walletPayments": "0.1.22"
                } },
                "futureValues": future_values
            } } } }
        })
    }

    fn at(time: &str) -> anyhow::Result<DateTime<Utc>> {
        Ok(DateTime::parse_from_rfc3339(time)?.with_timezone(&Utc))
    }

    fn version_of(versions: &[ExpectedPackageVersion], name: &str) -> Option<String> {
        versions
            .iter()
            .find(|v| v.package_name == name)
            .map(|v| v.version.clone())
    }

    #[test]
    fn the_initial_config_names_every_splice_package() -> anyhow::Result<()> {
        let dso = dso_with_schedule(serde_json::json!([]));

        let got = expected_splice_versions(&dso, at("2026-09-25T00:00:00Z")?)?;

        assert_eq!(got.len(), 6);
        assert_eq!(version_of(&got, "splice-amulet").as_deref(), Some("0.1.22"));
        assert_eq!(
            version_of(&got, "splice-amulet-name-service").as_deref(),
            Some("0.1.23")
        );
        assert_eq!(
            version_of(&got, "splice-wallet-payments").as_deref(),
            Some("0.1.22")
        );
        Ok(())
    }

    #[test]
    fn a_future_value_takes_effect_only_once_its_time_passes() -> anyhow::Result<()> {
        // Both tuple encodings appear in the wild: the JSON API writes a
        // `(Time, AmuletConfig)` as `{_1, _2}`, and some encoders as a pair.
        let dso = dso_with_schedule(serde_json::json!([
            { "_1": "2026-09-01T00:00:00Z", "_2": { "packageConfig": { "amulet": "0.1.23" } } },
            ["2026-10-01T00:00:00Z", { "packageConfig": { "amulet": "0.1.24" } }]
        ]));

        let before = expected_splice_versions(&dso, at("2026-08-31T00:00:00Z")?)?;
        let between = expected_splice_versions(&dso, at("2026-09-25T00:00:00Z")?)?;
        let after = expected_splice_versions(&dso, at("2026-10-02T00:00:00Z")?)?;

        assert_eq!(
            version_of(&before, "splice-amulet").as_deref(),
            Some("0.1.22")
        );
        assert_eq!(
            version_of(&between, "splice-amulet").as_deref(),
            Some("0.1.23")
        );
        assert_eq!(
            version_of(&after, "splice-amulet").as_deref(),
            Some("0.1.24")
        );
        Ok(())
    }

    #[test]
    fn a_response_without_amulet_rules_is_an_error() {
        let result = expected_splice_versions(&serde_json::json!({}), Utc::now());
        assert!(result.is_err());
    }

    #[test]
    fn the_dar_a_package_is_the_main_package_of_is_sent() {
        let dars = vec![dar("app", "app", "1.0.0"), dar("lib", "lib", "0.2.0")];

        let picked = pick_dar("lib", &dars).map(|d| d.main.as_str());

        assert_eq!(picked, Some("lib"));
    }

    #[test]
    fn a_dependency_is_sent_inside_a_dar_that_holds_it() {
        let dars = vec![dar("app", "app", "1.0.0")];

        let picked = pick_dar("lib", &dars).map(|d| d.main.as_str());

        assert_eq!(picked, Some("app"));
    }

    #[test]
    fn no_dar_holds_a_package_whose_dar_was_removed() {
        assert!(pick_dar("lib", &[]).is_none());
    }

    #[test]
    fn a_dar_file_is_named_like_an_upload() {
        assert_eq!(
            dar_filename(&dar("m", "utility-registry", "0.4.0")),
            "utility-registry-0.4.0.dar"
        );
        assert_eq!(
            dar_filename(&dar("m", "utility-registry", "")),
            "utility-registry.dar"
        );
        assert_eq!(dar_filename(&dar("m", "", "")), "m.dar");
    }

    /// A namespace long enough to look like a real Canton fingerprint, so the
    /// two uids below differ only in the part before `::`.
    const NS: &str = "1220cf0b33c716d8ea7a711353612aab36bf2c91674f69057e383328b35b52896ca2";

    fn vetted_result(
        participant_uid: &str,
        package_ids: &[&str],
    ) -> list_vetted_packages_response::Result {
        list_vetted_packages_response::Result {
            context: None,
            item: Some(
                canton_proto_rs::com::digitalasset::canton::protocol::v30::VettedPackages {
                    participant_uid: participant_uid.to_string(),
                    packages: package_ids
                        .iter()
                        .map(|id| VettedPackage {
                            package_id: (*id).to_string(),
                            valid_from_inclusive: None,
                            valid_until_exclusive: None,
                        })
                        .collect(),
                    ..Default::default()
                },
            ),
        }
    }

    #[test]
    fn a_prefix_colliding_participant_contributes_nothing() {
        // Canton splits `filter_participant` on `::` and matches each half as a
        // LIKE prefix, so asking for `participant` also returns `participant-2`.
        // Counting the neighbour's packages would report a peer as vetting
        // software it has never seen.
        let wanted = format!("participant::{NS}");
        let collider = format!("participant-2::{NS}");
        let now = Timestamp {
            seconds: 100,
            nanos: 0,
        };

        let ids = vetted_ids_of(
            vec![
                vetted_result(&collider, &["pkg-theirs"]),
                vetted_result(&wanted, &["pkg-mine"]),
            ],
            &wanted,
            &now,
        );

        assert_eq!(ids, vec!["pkg-mine".to_string()]);
    }

    #[test]
    fn only_the_exact_uid_survives_when_it_is_absent() {
        // The requested participant has vetted nothing, but a prefix collider
        // has. The answer must be empty, not the collider's list.
        let wanted = format!("participant::{NS}");
        let collider = format!("participant-2::{NS}");
        let now = Timestamp {
            seconds: 100,
            nanos: 0,
        };

        let ids = vetted_ids_of(
            vec![vetted_result(&collider, &["pkg-theirs"])],
            &wanted,
            &now,
        );

        assert!(ids.is_empty(), "{ids:?}");
    }

    #[test]
    fn vetted_ids_drop_invalid_windows_and_duplicates() {
        let wanted = format!("participant::{NS}");
        let now = Timestamp {
            seconds: 100,
            nanos: 0,
        };
        let ts = |seconds| Timestamp { seconds, nanos: 0 };

        let results = vec![list_vetted_packages_response::Result {
            context: None,
            item: Some(
                canton_proto_rs::com::digitalasset::canton::protocol::v30::VettedPackages {
                    participant_uid: wanted.clone(),
                    packages: vec![
                        VettedPackage {
                            package_id: "pkg-a".to_string(),
                            valid_from_inclusive: None,
                            valid_until_exclusive: None,
                        },
                        // duplicate of pkg-a
                        VettedPackage {
                            package_id: "pkg-a".to_string(),
                            valid_from_inclusive: None,
                            valid_until_exclusive: None,
                        },
                        // scheduled for the future, e.g. a Splice upgrade vetting
                        VettedPackage {
                            package_id: "pkg-future".to_string(),
                            valid_from_inclusive: Some(ts(101)),
                            valid_until_exclusive: None,
                        },
                        // already expired
                        VettedPackage {
                            package_id: "pkg-expired".to_string(),
                            valid_from_inclusive: None,
                            valid_until_exclusive: Some(ts(100)),
                        },
                    ],
                    ..Default::default()
                },
            ),
        }];

        assert_eq!(
            vetted_ids_of(results, &wanted, &now),
            vec!["pkg-a".to_string()]
        );
    }

    #[test]
    fn a_result_without_an_item_is_skipped() {
        let wanted = format!("participant::{NS}");
        let now = Timestamp {
            seconds: 100,
            nanos: 0,
        };

        let ids = vetted_ids_of(
            vec![
                list_vetted_packages_response::Result {
                    context: None,
                    item: None,
                },
                vetted_result(&wanted, &["pkg-a"]),
            ],
            &wanted,
            &now,
        );

        assert_eq!(ids, vec!["pkg-a".to_string()]);
    }

    #[test]
    fn test_package_name_prefix() {
        assert_eq!(
            package_name_prefix("#governance-core-v1-rc1"),
            "governance-core"
        );
        assert_eq!(
            package_name_prefix("#governance-action-v0"),
            "governance-action"
        );
        assert_eq!(
            package_name_prefix("#governance-utility-onboarding-v0-rc8"),
            "governance-utility-onboarding"
        );
        assert_eq!(package_name_prefix("cbtc-governance"), "cbtc-governance");
        assert_eq!(
            package_name_prefix("governance-core-v0-rc3"),
            "governance-core"
        );
        // `validator` starts with `v` but is not a version segment
        assert_eq!(package_name_prefix("#splice-validator"), "splice-validator");
    }

    #[test]
    fn test_matching_names() {
        let names = vec![
            "governance-core-v0-rc3".to_string(),
            "governance-core-v1-rc1".to_string(),
            "governance-core-extras-v1".to_string(),
            "cbtc-governance".to_string(),
        ];

        let matched = matching_names(&names, "governance-core");

        assert_eq!(
            matched.into_iter().collect::<Vec<_>>(),
            vec!["governance-core-v0-rc3", "governance-core-v1-rc1"]
        );
    }

    #[test]
    fn test_newest_matching_names_orders_newest_first() {
        let names = vec![
            "governance-core-v0-rc3".to_string(),
            "governance-core-v1-rc1".to_string(),
            "governance-core-v0-rc4".to_string(),
            "governance-core-extras-v1".to_string(),
            "cbtc-governance".to_string(),
        ];

        let ordered = newest_matching_names(&names, "governance-core");

        assert_eq!(
            ordered,
            vec![
                "governance-core-v1-rc1".to_string(),
                "governance-core-v0-rc4".to_string(),
                "governance-core-v0-rc3".to_string(),
            ]
        );
    }

    #[test]
    fn test_newest_matching_names_empty_when_family_absent() {
        let names = vec![
            "cbtc-governance".to_string(),
            "utility-registry-app-v0".to_string(),
        ];

        let ordered = newest_matching_names(&names, "governance-core");

        assert!(ordered.is_empty());
    }

    #[test]
    fn test_package_valid_at() {
        let ts = |seconds| Timestamp { seconds, nanos: 0 };
        let pkg = |from: Option<i64>, until: Option<i64>| VettedPackage {
            package_id: "pkg".to_string(),
            valid_from_inclusive: from.map(ts),
            valid_until_exclusive: until.map(ts),
        };
        let now = ts(100);

        assert!(package_valid_at(&pkg(None, None), &now));
        // `valid_from` is inclusive
        assert!(package_valid_at(&pkg(Some(100), None), &now));
        // scheduled for the future, e.g. a Splice upgrade vetting
        assert!(!package_valid_at(&pkg(Some(101), None), &now));
        assert!(package_valid_at(&pkg(None, Some(101)), &now));
        // `valid_until` is exclusive
        assert!(!package_valid_at(&pkg(None, Some(100)), &now));
        // expired
        assert!(!package_valid_at(&pkg(Some(0), Some(50)), &now));
    }

    #[test]
    fn test_version_tail() {
        assert_eq!(
            version_tail("governance-core-v1-rc1", "governance-core"),
            "1.1"
        );
        assert_eq!(
            version_tail("governance-core-v0-rc4", "governance-core"),
            "0.4"
        );
        assert_eq!(version_tail("cbtc-governance", "cbtc-governance"), "");
    }
}
