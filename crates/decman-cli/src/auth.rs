//! Login passwords, token lifetimes, and the on-disk token cache.
//!
//! The command mode runs once per invocation, so a login only lasts across
//! invocations if its tokens are kept on disk. The cache holds the access and
//! refresh tokens per API URL and username, readable by the current user only.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// A token counts as expired this many seconds before its real expiry, so a
/// request never leaves with a token that expires in flight.
const EXPIRY_MARGIN_SECS: i64 = 30;

/// Where the login password comes from.
#[derive(Clone, Debug)]
pub enum PasswordSource {
    /// The password itself.
    Literal(String),
    /// A shell command that prints the password on stdout, such as a password
    /// manager CLI. It runs only when a password login is needed.
    Command(String),
}

impl PasswordSource {
    /// The password, running the command if this source is one.
    ///
    /// # Errors
    ///
    /// Returns an error if the command cannot start, exits non-zero, or prints
    /// no password.
    pub fn resolve(&self) -> Result<String> {
        match self {
            Self::Literal(password) => Ok(password.clone()),
            Self::Command(command) => run_secret_command("password_command", command),
        }
    }
}

/// Run `command` through the platform shell and return its first stdout line.
/// Stdin and stderr stay attached to the terminal, so a password manager can
/// prompt the user and report its own errors. `label` names the setting in
/// errors; the secret itself never appears in one.
///
/// # Errors
///
/// Returns an error if the command cannot start, exits non-zero, or prints
/// nothing.
pub fn run_secret_command(label: &str, command: &str) -> Result<String> {
    let mut shell = if cfg!(windows) {
        let mut shell = Command::new("cmd");
        shell.arg("/C");
        shell
    } else {
        let mut shell = Command::new("sh");
        shell.arg("-c");
        shell
    };
    let output = shell
        .arg(command)
        .stdin(Stdio::inherit())
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("failed to run {label}"))?;
    if !output.status.success() {
        bail!("{label} exited with {}", output.status);
    }
    let stdout = String::from_utf8(output.stdout)
        .with_context(|| format!("{label} printed non-UTF-8 output"))?;
    let secret = stdout.lines().next().unwrap_or_default();
    if secret.is_empty() {
        bail!("{label} printed nothing");
    }
    Ok(secret.to_owned())
}

/// The OAuth2 token endpoint and client a login uses.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct AuthTarget {
    pub token_url: String,
    pub client_id: String,
    #[serde(default)]
    pub audience: Option<String>,
}

/// Token endpoint response for the password and refresh grants.
#[derive(Debug, Deserialize)]
pub struct TokenResponse {
    access_token: String,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    refresh_expires_in: Option<i64>,
}

/// An access token and, when the IdP issued one, a refresh token, with their
/// expiry times in unix seconds. `None` means the IdP gave no expiry.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub access_expires_at: Option<i64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub refresh_expires_at: Option<i64>,
}

impl TokenSet {
    /// Build a token set from a token endpoint response received at `now`.
    ///
    /// A refresh response can omit the refresh token. The previous refresh
    /// token then stays in use. Keycloak sends `refresh_expires_in: 0` for an
    /// offline token, which never expires.
    pub fn from_response(response: TokenResponse, now: i64, previous: Option<&Self>) -> Self {
        let expires_at = |seconds: Option<i64>| seconds.filter(|s| *s > 0).map(|s| now + s);
        let (refresh_token, refresh_expires_at) = match response.refresh_token {
            Some(token) => (Some(token), expires_at(response.refresh_expires_in)),
            None => previous.map_or((None, None), |previous| {
                (previous.refresh_token.clone(), previous.refresh_expires_at)
            }),
        };
        Self {
            access_token: response.access_token,
            access_expires_at: expires_at(response.expires_in),
            refresh_token,
            refresh_expires_at,
        }
    }

    /// Whether the access token is still good at `now`. A token without an
    /// expiry counts as good; the server's `401` then triggers a new login.
    pub fn access_valid(&self, now: i64) -> bool {
        is_unexpired(self.access_expires_at, now)
    }

    /// The refresh token, if there is one and it is still good at `now`.
    pub fn usable_refresh_token(&self, now: i64) -> Option<&str> {
        self.refresh_token
            .as_deref()
            .filter(|_| is_unexpired(self.refresh_expires_at, now))
    }

    /// Mark the access token as expired, keeping the refresh token.
    pub fn expire_access(&mut self) {
        self.access_expires_at = Some(0);
    }
}

fn is_unexpired(expires_at: Option<i64>, now: i64) -> bool {
    expires_at.is_none_or(|at| at - EXPIRY_MARGIN_SECS > now)
}

/// Current unix time in seconds (0 if the clock is before the epoch).
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or(0)
}

/// One cached login. The API URL and username are stored too, so a record is
/// never used for a different server or user, even if two file names collide.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CachedLogin {
    pub api_url: String,
    pub username: String,
    pub target: AuthTarget,
    pub tokens: TokenSet,
}

/// The token cache file for one API URL and username.
#[derive(Clone, Debug)]
pub struct TokenCache {
    path: PathBuf,
    api_url: String,
    username: String,
}

impl TokenCache {
    /// The cache file under the user's cache directory (`$XDG_CACHE_HOME`, else
    /// `~/.cache`), or `None` when neither is known.
    pub fn for_login(api_url: &str, username: &str) -> Option<Self> {
        let dir = std::env::var_os("XDG_CACHE_HOME")
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))?;
        Some(Self::in_dir(&dir.join("decman-cli"), api_url, username))
    }

    /// The cache file for this login inside `dir`.
    pub fn in_dir(dir: &Path, api_url: &str, username: &str) -> Self {
        let name: String = format!("{api_url}-{username}")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        Self {
            path: dir.join(format!("{name}.json")),
            api_url: api_url.to_owned(),
            username: username.to_owned(),
        }
    }

    /// The cached login, if the file exists, parses, and belongs to this API
    /// URL and username. A broken file counts as no cache, and so does a file
    /// or directory that another user could have written.
    pub fn load(&self) -> Option<CachedLogin> {
        let dir = self.path.parent()?;
        if !is_private(dir) || !is_private(&self.path) {
            return None;
        }
        let text = std::fs::read_to_string(&self.path).ok()?;
        let login: CachedLogin = serde_json::from_str(&text).ok()?;
        (login.api_url == self.api_url && login.username == self.username).then_some(login)
    }

    /// Write `target` and `tokens` to the cache file, readable by the current
    /// user only. The write goes to a temporary file first, so a parallel
    /// invocation never reads half a file.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory or file cannot be written.
    pub fn save(&self, target: &AuthTarget, tokens: &TokenSet) -> Result<()> {
        let login = CachedLogin {
            api_url: self.api_url.clone(),
            username: self.username.clone(),
            target: target.clone(),
            tokens: tokens.clone(),
        };
        let dir = self
            .path
            .parent()
            .context("token cache path has no directory")?;
        create_private_dir(dir)?;
        if !is_private(dir) {
            bail!(
                "the token cache directory {} is a link or open to other users; \
                 run chmod 700 on it, or pass --no-token-cache",
                dir.display()
            );
        }
        let temp = self
            .path
            .with_extension(format!("tmp.{}", std::process::id()));
        write_private_file(&temp, &serde_json::to_vec(&login)?)?;
        std::fs::rename(&temp, &self.path).context("failed to move the token cache into place")?;
        Ok(())
    }

    /// Delete the cache file. Returns whether a file was there.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be deleted.
    pub fn clear(&self) -> Result<bool> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error).context("failed to delete the token cache"),
        }
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .context("failed to create the token cache directory")
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).context("failed to create the token cache directory")
}

/// Whether `path` is a real file or directory that only its owner can read or
/// write. A symlink never counts as private.
#[cfg(unix)]
fn is_private(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .is_ok_and(|meta| !meta.file_type().is_symlink() && meta.permissions().mode() & 0o077 == 0)
}

#[cfg(not(unix))]
fn is_private(_path: &Path) -> bool {
    true
}

/// Write `bytes` to a new file at `path`, readable by the owner only. The file
/// must not exist, so a planted file or symlink makes the write fail instead
/// of being followed. A leftover from an earlier crashed run is removed first.
#[cfg(unix)]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let open = || {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    };
    let mut file = match open() {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(path).context("failed to remove a stale token cache file")?;
            open()
        }
        other => other,
    }
    .context("failed to open the token cache file")?;
    file.write_all(bytes)
        .context("failed to write the token cache file")
}

#[cfg(not(unix))]
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).context("failed to write the token cache file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(json: &str) -> TokenResponse {
        serde_json::from_str(json).unwrap()
    }

    fn target() -> AuthTarget {
        AuthTarget {
            token_url: "https://idp/token".to_owned(),
            client_id: "app".to_owned(),
            audience: None,
        }
    }

    #[test]
    fn token_set_turns_lifetimes_into_expiry_times() {
        // Arrange
        let json = r#"{"access_token":"a","expires_in":300,
            "refresh_token":"r","refresh_expires_in":1800}"#;

        // Act
        let tokens = TokenSet::from_response(response(json), 1_000, None);

        // Assert
        assert_eq!(tokens.access_expires_at, Some(1_300));
        assert_eq!(tokens.refresh_token.as_deref(), Some("r"));
        assert_eq!(tokens.refresh_expires_at, Some(2_800));
    }

    #[test]
    fn zero_refresh_lifetime_means_no_expiry() {
        // Arrange
        let json = r#"{"access_token":"a","refresh_token":"r","refresh_expires_in":0}"#;

        // Act
        let tokens = TokenSet::from_response(response(json), 1_000, None);

        // Assert
        assert_eq!(tokens.refresh_expires_at, None);
        assert_eq!(tokens.usable_refresh_token(i64::MAX - 100), Some("r"));
    }

    #[test]
    fn refresh_without_new_refresh_token_keeps_the_old_one() {
        // Arrange
        let previous = TokenSet {
            access_token: "old".to_owned(),
            access_expires_at: Some(0),
            refresh_token: Some("r1".to_owned()),
            refresh_expires_at: Some(9_000),
        };

        // Act
        let tokens = TokenSet::from_response(
            response(r#"{"access_token":"new","expires_in":60}"#),
            1_000,
            Some(&previous),
        );

        // Assert
        assert_eq!(tokens.access_token, "new");
        assert_eq!(tokens.refresh_token.as_deref(), Some("r1"));
        assert_eq!(tokens.refresh_expires_at, Some(9_000));
    }

    #[test]
    fn tokens_expire_a_margin_before_their_expiry_time() {
        // Arrange
        let mut tokens = TokenSet {
            access_token: "a".to_owned(),
            access_expires_at: Some(1_000),
            refresh_token: Some("r".to_owned()),
            refresh_expires_at: Some(1_000),
        };

        // Act / Assert
        assert!(tokens.access_valid(1_000 - EXPIRY_MARGIN_SECS - 1));
        assert!(!tokens.access_valid(1_000 - EXPIRY_MARGIN_SECS));
        assert_eq!(tokens.usable_refresh_token(1_000), None);
        tokens.expire_access();
        assert!(!tokens.access_valid(1));
    }

    #[test]
    fn cache_round_trips_and_is_private() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let cache = TokenCache::in_dir(&dir.path().join("decman-cli"), "https://a", "alice");
        let tokens = TokenSet {
            access_token: "a".to_owned(),
            access_expires_at: Some(5),
            refresh_token: None,
            refresh_expires_at: None,
        };

        // Act
        cache.save(&target(), &tokens).unwrap();
        let loaded = cache.load().unwrap();

        // Assert
        assert_eq!(loaded.tokens, tokens);
        assert_eq!(loaded.target, target());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&cache.path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(cache.clear().unwrap());
        assert!(!cache.clear().unwrap());
        assert!(cache.load().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn cache_in_a_directory_open_to_others_is_refused() {
        // Arrange
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("decman-cli");
        let cache = TokenCache::in_dir(&cache_dir, "https://a", "alice");
        let tokens = TokenSet {
            access_token: "a".to_owned(),
            access_expires_at: None,
            refresh_token: None,
            refresh_expires_at: None,
        };
        cache.save(&target(), &tokens).unwrap();
        std::fs::set_permissions(&cache_dir, std::fs::Permissions::from_mode(0o777)).unwrap();

        // Act
        let loaded = cache.load();
        let saved = cache.save(&target(), &tokens);

        // Assert
        assert!(loaded.is_none());
        assert!(
            saved
                .unwrap_err()
                .to_string()
                .contains("open to other users")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_cache_file_is_not_read() {
        // Arrange
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path().join("decman-cli");
        let cache = TokenCache::in_dir(&cache_dir, "https://a", "alice");
        let tokens = TokenSet {
            access_token: "a".to_owned(),
            access_expires_at: None,
            refresh_token: None,
            refresh_expires_at: None,
        };
        cache.save(&target(), &tokens).unwrap();
        let real = dir.path().join("elsewhere.json");
        std::fs::rename(&cache.path, &real).unwrap();
        std::os::unix::fs::symlink(&real, &cache.path).unwrap();

        // Act / Assert
        assert!(cache.load().is_none());
    }

    #[test]
    fn cache_ignores_a_record_for_another_server() {
        // Arrange — two logins whose file names collide after sanitizing.
        let dir = tempfile::tempdir().unwrap();
        let first = TokenCache::in_dir(&dir.path().join("cache"), "https://a.b", "alice");
        let second = TokenCache::in_dir(&dir.path().join("cache"), "https://a_b", "alice");
        assert_eq!(first.path, second.path);
        let tokens = TokenSet {
            access_token: "a".to_owned(),
            access_expires_at: None,
            refresh_token: None,
            refresh_expires_at: None,
        };

        // Act
        first.save(&target(), &tokens).unwrap();

        // Assert
        assert!(first.load().is_some());
        assert!(second.load().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn password_command_returns_the_first_stdout_line() {
        // Arrange
        let source = PasswordSource::Command("printf 'hunter2\\nignored\\n'".to_owned());

        // Act
        let password = source.resolve().unwrap();

        // Assert
        assert_eq!(password, "hunter2");
    }

    #[cfg(unix)]
    #[test]
    fn failing_password_command_is_an_error() {
        // Arrange
        let source = PasswordSource::Command("exit 3".to_owned());

        // Act
        let error = source.resolve().unwrap_err();

        // Assert
        assert!(
            error.to_string().contains("password_command exited"),
            "{error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn empty_password_command_output_is_an_error() {
        // Arrange
        let source = PasswordSource::Command("true".to_owned());

        // Act
        let error = source.resolve().unwrap_err();

        // Assert
        assert!(error.to_string().contains("printed nothing"), "{error}");
    }
}
