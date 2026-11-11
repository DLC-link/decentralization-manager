//! Login: auth discovery, the password grant, refresh and the token cache.

use anyhow::{Context, Result, bail};
use common::types::AuthConfigResponse;

use super::client::{DecmanClient, ensure_secure_url, for_terminal};
use crate::auth::{AuthTarget, TokenCache, TokenResponse, TokenSet, now_secs};

/// Build a Keycloak token endpoint from its host and realm.
fn keycloak_token_url(host: &str, realm: &str) -> String {
    format!(
        "{host}/realms/{realm}/protocol/openid-connect/token",
        host = host.trim_end_matches('/'),
    )
}

impl DecmanClient {
    /// Whether `target` agrees with the explicit token URL and client id.
    pub(super) fn matches_overrides(&self, target: &AuthTarget) -> bool {
        let agrees = |explicit: &Option<String>, cached: &str| {
            explicit.as_deref().is_none_or(|value| value == cached)
        };
        agrees(&self.auth.token_url, &target.token_url)
            && agrees(&self.auth.client_id, &target.client_id)
            && self
                .auth
                .audience
                .as_ref()
                .is_none_or(|audience| target.audience.as_ref() == Some(audience))
    }

    /// Take a newer login from the cache file. A parallel invocation can have
    /// refreshed it since this client loaded it, and a second refresh with
    /// the same refresh token can fail. So a new refresh token is taken even
    /// when its access token has expired. The access token this client
    /// already holds, for example one the server just rejected, is never
    /// used again.
    fn adopt_newer_cached_login(&mut self, now: i64) {
        let Some(mut login) = self.cache.as_ref().and_then(TokenCache::load) else {
            return;
        };
        if !self.matches_overrides(&login.target) {
            return;
        }
        let held = self.tokens.as_ref();
        let same_access =
            held.is_some_and(|tokens| tokens.access_token == login.tokens.access_token);
        let newer_access = !same_access && login.tokens.access_valid(now);
        let newer_refresh = login.tokens.usable_refresh_token(now).is_some()
            && held.is_none_or(|tokens| tokens.refresh_token != login.tokens.refresh_token);
        if !newer_access && !newer_refresh {
            return;
        }
        if same_access {
            login.tokens.expire_access();
        }
        self.target = Some(Some(login.target));
        self.target_from_cache = true;
        self.tokens = Some(login.tokens);
    }

    /// Log in now, or reuse a valid or refreshable cached login, and return
    /// the access token's expiry time (`None` when it has none, or when the
    /// node has auth disabled).
    ///
    /// # Errors
    ///
    /// Returns an error if discovery or every login attempt fails.
    pub fn login(&mut self) -> Result<Option<i64>> {
        self.authenticate()?;
        Ok(self
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.access_expires_at))
    }

    /// Delete the cached login. Returns whether there was one.
    ///
    /// # Errors
    ///
    /// Returns an error if the cache file exists but cannot be deleted.
    pub fn logout(&mut self) -> Result<bool> {
        self.tokens = None;
        match &self.cache {
            Some(cache) => cache.clear(),
            None => Ok(false),
        }
    }

    /// Make sure a usable access token is held, when the node requires one.
    ///
    /// The order is: a still-valid access token, then the refresh grant, then
    /// a password login. A failed refresh (for example an expired session)
    /// falls through to the password login. A new token goes to the cache.
    pub(super) fn authenticate(&mut self) -> Result<()> {
        let now = now_secs();
        if self
            .tokens
            .as_ref()
            .is_some_and(|tokens| tokens.access_valid(now))
        {
            return Ok(());
        }
        // Hold the cache lock until the new login is saved, so a parallel
        // invocation waits and then takes the saved login. Two refreshes with
        // the same single-use refresh token would make one of them fail.
        let _lock = match &self.cache {
            Some(cache) => cache
                .lock()
                .map_err(|error| {
                    self.warning(&format!("the token cache is not locked: {error:#}"));
                })
                .ok(),
            None => None,
        };
        self.adopt_newer_cached_login(now);
        if self
            .tokens
            .as_ref()
            .is_some_and(|tokens| tokens.access_valid(now))
        {
            return Ok(());
        }
        let target = match self.target.clone() {
            Some(target) => target,
            None => {
                let target = self.resolve_auth_target()?;
                self.target = Some(target.clone());
                target
            }
        };
        let Some(target) = target else {
            self.tokens = None;
            return Ok(());
        };

        let previous = self.tokens.take();
        let refreshed = previous
            .as_ref()
            .and_then(|tokens| tokens.usable_refresh_token(now))
            .and_then(|refresh| {
                self.refresh_grant(&target, refresh, previous.as_ref())
                    .map_err(|error| self.warning(&format!("token refresh failed: {error:#}")))
                    .ok()
            });
        let (target, tokens) = match refreshed {
            Some(tokens) => (target, tokens),
            None => self.password_login(target)?,
        };
        if let Some(cache) = &self.cache
            && let Err(error) = cache.save(&target, &tokens)
        {
            // A cache write failure only costs a login on the next call.
            self.warning(&format!("the login was not cached: {error:#}"));
        }
        self.tokens = Some(tokens);
        Ok(())
    }

    /// Print `message` on stderr when warnings are on.
    fn warning(&self, message: &str) {
        if self.warn {
            eprintln!("warning: {}", for_terminal(message));
        }
    }

    /// Log in with the password. A token endpoint from the cache can be out
    /// of date, for example after the node moved to another IdP. The password
    /// must never go to an old host, so a cached target is discovered again
    /// before the password is sent.
    fn password_login(&mut self, target: AuthTarget) -> Result<(AuthTarget, TokenSet)> {
        let target = if self.target_from_cache {
            self.target_from_cache = false;
            let fresh = self.resolve_auth_target()?;
            self.target = Some(fresh.clone());
            fresh.context("this node no longer requires a login; run logout")?
        } else {
            target
        };
        let password = self.resolve_password()?;
        let tokens = self.password_grant(&target, &password)?;
        Ok((target, tokens))
    }

    /// The configured password, running the password command on first use.
    fn resolve_password(&mut self) -> Result<String> {
        if let Some(password) = &self.password {
            return Ok(password.clone());
        }
        let password = self
            .auth
            .password
            .as_ref()
            .context("this node requires a login, but no password or password_command is set")?
            .resolve()?;
        self.password = Some(password.clone());
        Ok(password)
    }

    /// The current access token, if any.
    pub(super) fn access_token(&self) -> Option<&str> {
        self.tokens
            .as_ref()
            .map(|tokens| tokens.access_token.as_str())
    }

    /// Forget the access token after a `401`, keeping the refresh token, so the
    /// next [`Self::authenticate`] refreshes before it logs in again.
    pub(super) fn drop_access_token(&mut self) {
        if let Some(tokens) = &mut self.tokens {
            tokens.expire_access();
        }
    }

    /// Resolve the token endpoint, client id, and audience.
    ///
    /// Explicit `token_url` + `client_id` overrides skip the network call;
    /// otherwise `/auth-config` is queried (returning `None` when the node has
    /// auth disabled, e.g. test mode).
    fn resolve_auth_target(&self) -> Result<Option<AuthTarget>> {
        if let (Some(token_url), Some(client_id)) = (&self.auth.token_url, &self.auth.client_id) {
            return Ok(Some(AuthTarget {
                token_url: token_url.clone(),
                client_id: client_id.clone(),
                audience: self.auth.audience.clone(),
            }));
        }

        let config = self.fetch_auth_config()?;
        if !config.auth_required {
            return Ok(None);
        }

        // Auth0 takes precedence over Keycloak, mirroring the backend.
        if let Some(domain) = config.auth0_domain {
            let client_id = self
                .auth
                .client_id
                .clone()
                .or(config.auth0_client_id)
                .context("Auth0 client id missing from /auth-config")?;
            return Ok(Some(AuthTarget {
                token_url: self
                    .auth
                    .token_url
                    .clone()
                    .unwrap_or_else(|| format!("https://{domain}/oauth/token")),
                client_id,
                audience: self.auth.audience.clone().or(config.auth0_audience),
            }));
        }

        let client_id = self
            .auth
            .client_id
            .clone()
            .or(config.keycloak_client_id)
            .context("Keycloak client id missing from /auth-config")?;
        let token_url = match &self.auth.token_url {
            Some(url) => url.clone(),
            None => {
                let host = config
                    .keycloak_host
                    .context("Keycloak host missing from /auth-config")?;
                let realm = config
                    .keycloak_realm
                    .context("Keycloak realm missing from /auth-config")?;
                keycloak_token_url(&host, &realm)
            }
        };

        Ok(Some(AuthTarget {
            token_url,
            client_id,
            audience: self.auth.audience.clone(),
        }))
    }

    /// Fetch the public `/auth-config` to discover IdP settings.
    fn fetch_auth_config(&self) -> Result<AuthConfigResponse> {
        let url = format!("{base}/auth-config", base = self.base_url);
        let response = self
            .http
            .get(&url)
            .send()
            .with_context(|| format!("request to {url} failed"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!("/auth-config returned {status}: {body}");
        }

        response
            .json()
            .context("failed to parse /auth-config response")
    }

    /// Mint an access token via the OAuth2 password grant.
    fn password_grant(&self, target: &AuthTarget, password: &str) -> Result<TokenSet> {
        ensure_secure_url(&target.token_url)?;
        let mut form = vec![
            ("grant_type", "password"),
            ("client_id", target.client_id.as_str()),
            ("username", self.auth.username.as_str()),
            ("password", password),
        ];
        if let Some(secret) = &self.auth.client_secret {
            form.push(("client_secret", secret.as_str()));
        }
        if let Some(audience) = &target.audience {
            form.push(("audience", audience.as_str()));
        }
        if let Some(scope) = &self.auth.scope {
            form.push(("scope", scope.as_str()));
        }

        let response = self
            .http
            .post(&target.token_url)
            .form(&form)
            .send()
            .with_context(|| format!("token request to {url} failed", url = target.token_url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            // The login (password / direct-access) grant needs the OAuth client
            // to permit it — surface an actionable hint for this common case.
            if body.contains("unauthorized_client") || body.contains("direct access grants") {
                bail!(
                    "token endpoint returned {status}: {body} — this client does not allow the \
                     password grant. Enable \"Direct Access Grants\" on the client in Keycloak, or \
                     set oauth_client_id for this profile to a client that allows it."
                );
            }
            bail!("token endpoint returned {status}: {body}");
        }

        let token: TokenResponse = response.json().context("failed to parse token response")?;
        Ok(TokenSet::from_response(token, now_secs(), None))
    }

    /// Exchange a refresh token for a new access token.
    fn refresh_grant(
        &self,
        target: &AuthTarget,
        refresh_token: &str,
        previous: Option<&TokenSet>,
    ) -> Result<TokenSet> {
        ensure_secure_url(&target.token_url)?;
        let mut form = vec![
            ("grant_type", "refresh_token"),
            ("client_id", target.client_id.as_str()),
            ("refresh_token", refresh_token),
        ];
        if let Some(secret) = &self.auth.client_secret {
            form.push(("client_secret", secret.as_str()));
        }
        let response = self
            .http
            .post(&target.token_url)
            .form(&form)
            .send()
            .with_context(|| format!("token request to {url} failed", url = target.token_url))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            bail!("refresh grant returned {status}: {body}");
        }
        let token: TokenResponse = response.json().context("failed to parse token response")?;
        Ok(TokenSet::from_response(token, now_secs(), previous))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::AuthSettings;
    use crate::auth::PasswordSource;

    fn auth_settings() -> AuthSettings {
        AuthSettings {
            username: "alice".to_owned(),
            password: Some(PasswordSource::Literal("secret".to_owned())),
            token_url: None,
            client_id: None,
            client_secret: None,
            audience: None,
            scope: None,
        }
    }

    #[test]
    fn keycloak_token_url_trims_trailing_slash() {
        assert_eq!(
            keycloak_token_url("https://kc.example/", "my-realm"),
            "https://kc.example/realms/my-realm/protocol/openid-connect/token"
        );
    }

    #[test]
    fn explicit_overrides_skip_discovery() {
        // Arrange — both overrides present, so no /auth-config call is made.
        let auth = AuthSettings {
            token_url: Some("https://idp/token".to_owned()),
            client_id: Some("decman-web".to_owned()),
            ..auth_settings()
        };
        let client = DecmanClient::new("https://api.invalid".to_owned(), auth).unwrap();

        // Act
        let target = client.resolve_auth_target().unwrap().unwrap();

        // Assert
        assert_eq!(target.token_url, "https://idp/token");
        assert_eq!(target.client_id, "decman-web");
    }
}
