//! Build identity for this binary.
//!
//! There are two distinct notions of "version" here, and they must not be
//! conflated:
//!
//! * [`SEMVER`] is the Cargo package version. It is the *compatibility*
//!   version peers exchange over Noise and gate on (`MIN_PEER_VERSION`), so it
//!   must stay a parseable semver and is compiled in via `CARGO_PKG_VERSION`.
//! * [`build_version`] and [`build_time`] are *display only*. CI passes the
//!   pushed image tag (releases) or short commit SHA (per-commit dev images)
//!   plus a build timestamp as Docker `--build-arg`s, which the runtime
//!   Dockerfile turns into the `DECPM_BUILD_VERSION` / `DECPM_BUILD_TIME` env
//!   vars. Because the same value that tags the image fills `build_version`,
//!   what the UI shows equals the image tag by construction.
//!
//! Outside CI (a plain `cargo run`, local `docker build`) the env vars are
//! unset, so `build_version` falls back to `<semver>-dev` and `build_time` is
//! `None`.

use std::sync::LazyLock;

/// Cargo package semver — the wire/compatibility version peers gate on.
pub const SEMVER: &str = env!("CARGO_PKG_VERSION");

static BUILD_VERSION: LazyLock<String> =
    LazyLock::new(|| read_env("DECPM_BUILD_VERSION").unwrap_or_else(|| format!("{SEMVER}-dev")));

static BUILD_TIME: LazyLock<Option<String>> = LazyLock::new(|| read_env("DECPM_BUILD_TIME"));

/// Display build identifier: the git tag on release images, the short SHA on
/// per-commit images, or `<semver>-dev` when built/run outside CI.
pub fn build_version() -> &'static str {
    &BUILD_VERSION
}

/// When the image was built (RFC 3339), if CI stamped it. `None` outside CI.
pub fn build_time() -> Option<&'static str> {
    BUILD_TIME.as_deref()
}

/// Read an env var, treating unset or blank (`""`, whitespace) as absent — a
/// build-arg that isn't passed still materializes as an empty `ENV`, which we
/// want to behave the same as unset.
fn read_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}
