use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result};
use canton_proto_rs::com::digitalasset::canton::admin::participant::v30::{
    ListPackagesRequest, package_service_client::PackageServiceClient,
};

use crate::config::NodeConfig;

use super::queries::compare_versions;

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
    let mut client = PackageServiceClient::connect(config.admin_api_url())
        .await
        .context("Failed to connect to participant Admin API")?;
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
    let mut client = PackageServiceClient::connect(config.admin_api_url())
        .await
        .context("Failed to connect to participant Admin API")?;
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
            package_name_prefix("#bitsafe-vault-governance-v0-rc8"),
            "bitsafe-vault-governance"
        );
        assert_eq!(package_name_prefix("cbtc-governance"), "cbtc-governance");
        assert_eq!(
            package_name_prefix("governance-core-v0-rc3"),
            "governance-core"
        );
        // `vault` starts with `v` but is not a version segment
        assert_eq!(package_name_prefix("#bitsafe-vault"), "bitsafe-vault");
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
            "bitsafe-vault-v1".to_string(),
        ];

        let ordered = newest_matching_names(&names, "governance-core");

        assert!(ordered.is_empty());
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
