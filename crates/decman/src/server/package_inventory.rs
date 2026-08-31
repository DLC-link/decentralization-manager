use std::{
    collections::{BTreeSet, HashMap},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use canton_proto_rs::com::digitalasset::canton::{
    admin::participant::v30::{ListPackagesRequest, package_service_client::PackageServiceClient},
    protocol::v30::{enums::TopologyChangeOp, vetted_packages::VettedPackage},
    topology::admin::v30::{
        BaseQuery, ListVettedPackagesRequest,
        topology_manager_read_service_client::TopologyManagerReadServiceClient,
    },
};
use prost_types::Timestamp;

use crate::{config::NodeConfig, utils, workflow::topology};

use super::{queries::compare_versions, types::VettedPackageInfo};

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
    let synchronizer_id = utils::get_synchronizer_id(config).await?;
    let channel = config
        .admin_channel()
        .await
        .context("Failed to connect to participant Admin API")?;
    let mut client = TopologyManagerReadServiceClient::new(channel)
        .max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let response = client
        .list_vetted_packages(tonic::Request::new(ListVettedPackagesRequest {
            base_query: Some(BaseQuery {
                operation: TopologyChangeOp::AddReplace as i32,
                ..topology::head_state_query(&synchronizer_id)
            }),
            filter_participant: config.participant_id().to_string(),
        }))
        .await
        .context("Failed to list vetted packages")?
        .into_inner();

    let descriptions = fetch_package_descriptions(config).await?;
    let now = now_timestamp();

    let mut seen = std::collections::HashSet::new();
    let mut vetted = Vec::new();
    for result in response.results {
        let Some(item) = result.item else { continue };
        for package in item.packages {
            if !package_valid_at(&package, &now) {
                continue;
            }
            if !seen.insert(package.package_id.clone()) {
                continue;
            }
            let (name, version) = descriptions
                .get(&package.package_id)
                .cloned()
                .unwrap_or_default();
            vetted.push(VettedPackageInfo {
                package_id: package.package_id,
                package_name: name,
                package_version: version,
            });
        }
    }

    Ok(vetted)
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
