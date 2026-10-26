//! OAuth2 authentication, token refresh, and credential persistence.
//!
//! OGS's public OAuth application is configured for the resource-owner
//! password grant. The token endpoint requires a `User-Agent` header; without
//! it Cloudflare rejects the request with `403` / error `1010`.
//!
//! `AuthTokens` is the REST credential. It is **not** the Realtime credential:
//! the socket authenticates with the `user_jwt` returned by `GET /ui/config`.

use crate::config::{self, StoredCredentials};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{Mutex, RwLock};

/// OAuth2 token endpoint. The trailing slash is mandatory.
pub const TOKEN_URL: &str = "https://online-go.com/oauth2/token/";
/// Public OGS OAuth application used by this client.
const DEFAULT_CLIENT_ID: &str = "5KdXwbxtrEOZWcukz4rWbITUyhAmEy1b6twhNTFz";
/// A browser-shaped User-Agent; Cloudflare blocks empty or bot-like ones.
pub const USER_AGENT: &str =
    concat!("Mozilla/5.0 (compatible; ogs-tui/", env!("CARGO_PKG_VERSION"), ")");
/// Treat a token as expired this long before its stated expiry.
const EXPIRY_MARGIN: Duration = Duration::from_secs(300);

/// Returns the configured OAuth client id, allowing an override for forks.
pub fn client_id() -> &'static str {
    static CLIENT_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CLIENT_ID
        .get_or_init(|| {
            std::env::var("OGS_CLIENT_ID").unwrap_or_else(|_| DEFAULT_CLIENT_ID.to_string())
        })
        .as_str()
}

/// Shared HTTP client for token and REST traffic.
pub fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("failed to build the HTTP client")
    })
}

/// A REST access token plus the refresh token needed to renew it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthTokens {
    pub username: String,
    pub user_id: Option<i64>,
    pub access_token: String,
    pub refresh_token: String,
    /// Absolute expiry, already reduced by [`EXPIRY_MARGIN`].
    pub expires_at: SystemTime,
}

impl AuthTokens {
    /// At least one minute before expiry, or expired.
    pub fn is_expired(&self) -> bool {
        SystemTime::now() + Duration::from_secs(60) >= self.expires_at
    }

    pub fn expires_at_unix(&self) -> u64 {
        self.expires_at
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    }

    /// Builds tokens from a persisted record.
    pub fn from_stored(stored: StoredCredentials) -> Self {
        Self {
            username: stored.username,
            user_id: stored.user_id,
            access_token: stored.access_token,
            refresh_token: stored.refresh_token,
            expires_at: UNIX_EPOCH + Duration::from_secs(stored.expires_at_unix),
        }
    }

    /// Serializes tokens for disk storage.
    pub fn to_stored(&self) -> StoredCredentials {
        StoredCredentials {
            username: self.username.clone(),
            user_id: self.user_id,
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            expires_at_unix: self.expires_at_unix(),
            client_id: Some(client_id().to_string()),
        }
    }
}

/// Logs in with the resource-owner password grant.
pub async fn login(username: &str, password: &str) -> Result<AuthTokens, String> {
    let form = [
        ("grant_type", "password".to_string()),
        ("username", username.to_string()),
        ("password", password.to_string()),
        ("client_id", client_id().to_string()),
    ];
    let response = post_token(&form).await?;
    tokens_from_response(response, username, None)
}

/// Refreshes an existing token set.
pub async fn refresh(tokens: &AuthTokens) -> Result<AuthTokens, String> {
    let form = [
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", tokens.refresh_token.clone()),
        ("client_id", client_id().to_string()),
    ];
    let response = post_token(&form).await?;
    tokens_from_response(response, &tokens.username, Some(tokens.refresh_token.as_str()))
}

async fn post_token(form: &[(&str, String)]) -> Result<Value, String> {
    let response = http_client()
        .post(TOKEN_URL)
        .form(form)
        .send()
        .await
        .map_err(|error| format!("token request failed: {error}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("token request failed ({status}): {body}"));
    }
    response
        .json()
        .await
        .map_err(|error| format!("token response was not JSON: {error}"))
}

/// Parses a token endpoint response. `fallback_refresh` lets a refresh
/// response omit the refresh token and reuse the previous one.
pub(crate) fn tokens_from_response(
    response: Value,
    username: &str,
    fallback_refresh: Option<&str>,
) -> Result<AuthTokens, String> {
    let access_token = response
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "token response had no access_token".to_string())?
        .to_string();
    let refresh_token = response
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| fallback_refresh.map(str::to_string))
        .ok_or_else(|| "token response had no refresh_token".to_string())?;
    let expires_in = response
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(2_592_000);
    let margin = EXPIRY_MARGIN
        .as_secs()
        .min(expires_in.saturating_sub(60));
    let lifetime = expires_in.saturating_sub(margin);
    let expires_at = SystemTime::now() + Duration::from_secs(lifetime);

    Ok(AuthTokens {
        username: username.to_string(),
        user_id: None,
        access_token,
        refresh_token,
        expires_at,
    })
}

/// Decodes a JWT payload without verifying its signature.
///
/// This is only used to catch the "guest JWT" failure mode before opening the
/// socket: the server's `/ui/config` response must carry a token whose
/// `anonymous` claim is false.
pub fn jwt_claims(token: &str) -> Option<Value> {
    use base64::Engine;
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(parts[1]))
        .ok()?;
    serde_json::from_slice(&payload).ok()
}

/// Returns the JWT's `anonymous` claim when the token is decodable.
pub fn jwt_is_anonymous(token: &str) -> Option<bool> {
    jwt_claims(token)?
        .get("anonymous")
        .and_then(Value::as_bool)
}

/// Shared, refreshable authentication state.
///
/// Cloning the handle is cheap; all clones observe the same tokens and the
/// same refresh lock. Only one refresh request is in flight at a time even when
/// several REST calls hit a 401 concurrently.
#[derive(Clone)]
pub struct AuthSession {
    state: Arc<RwLock<AuthTokens>>,
    refresh_lock: Arc<Mutex<()>>,
    persist_path: Option<PathBuf>,
}

impl AuthSession {
    /// Creates a session and optionally adopts a persistence path.
    pub fn new(tokens: AuthTokens, persist_path: Option<PathBuf>) -> Self {
        Self {
            state: Arc::new(RwLock::new(tokens)),
            refresh_lock: Arc::new(Mutex::new(())),
            persist_path,
        }
    }

    /// Creates a session from a persisted record.
    pub fn from_stored(stored: StoredCredentials, persist_path: Option<PathBuf>) -> Self {
        Self::new(AuthTokens::from_stored(stored), persist_path)
    }

    /// The default persistence path (`$XDG_CONFIG_HOME/ogs-tui/...`).
    pub fn default_path() -> PathBuf {
        config::credentials_path()
    }

    pub async fn tokens(&self) -> AuthTokens {
        self.state.read().await.clone()
    }

    pub async fn username(&self) -> String {
        self.state.read().await.username.clone()
    }

    pub async fn user_id(&self) -> Option<i64> {
        self.state.read().await.user_id
    }

    pub async fn access_token(&self) -> String {
        self.state.read().await.access_token.clone()
    }

    /// Records the numeric user id learned from `GET /me`.
    pub async fn set_user_id(&self, user_id: i64) {
        let mut state = self.state.write().await;
        if state.user_id != Some(user_id) {
            state.user_id = Some(user_id);
        }
    }

    /// Refreshes the token if it is close to expiry.
    pub async fn ensure_valid(&self) -> Result<(), String> {
        if !self.state.read().await.is_expired() {
            return Ok(());
        }
        self.refresh().await
    }

    /// Forces a refresh, serialized against other refreshes.
    pub async fn refresh(&self) -> Result<(), String> {
        let _guard = self.refresh_lock.lock().await;
        // Another caller may have refreshed while we waited.
        let snapshot = self.state.read().await.clone();
        if !snapshot.is_expired() {
            return Ok(());
        }
        let refreshed = refresh(&snapshot).await?;
        *self.state.write().await = refreshed;
        self.persist().await
    }

    /// Forces a refresh even if the clock says the token is still valid, used
    /// after a 401 response.
    pub async fn refresh_after_unauthorized(&self) -> Result<(), String> {
        let _guard = self.refresh_lock.lock().await;
        let snapshot = self.state.read().await.clone();
        let refreshed = refresh(&snapshot).await?;
        *self.state.write().await = refreshed;
        self.persist().await
    }

    /// Writes the current tokens to disk, if a path was configured.
    pub async fn persist(&self) -> Result<(), String> {
        let Some(path) = &self.persist_path else {
            return Ok(());
        };
        let stored = self.state.read().await.to_stored();
        config::save_credentials_to(path, &stored)
            .map_err(|error| format!("could not save credentials: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ogs-tui-auth-{}-{}-{tag}.json",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn parses_a_password_grant_response() {
        let tokens = tokens_from_response(
            json!({
                "access_token": "abc",
                "refresh_token": "def",
                "expires_in": 2592000,
                "token_type": "Bearer",
            }),
            "sora",
            None,
        )
        .unwrap();
        assert_eq!(tokens.access_token, "abc");
        assert_eq!(tokens.refresh_token, "def");
        assert_eq!(tokens.username, "sora");
        assert!(!tokens.is_expired());
        assert_eq!(tokens.user_id, None);
    }

    #[test]
    fn refresh_response_can_omit_the_refresh_token() {
        let tokens = tokens_from_response(
            json!({"access_token": "new", "expires_in": 100}),
            "sora",
            Some("old-refresh"),
        )
        .unwrap();
        assert_eq!(tokens.access_token, "new");
        assert_eq!(tokens.refresh_token, "old-refresh");
    }

    #[test]
    fn missing_access_token_is_an_error() {
        assert!(tokens_from_response(json!({"refresh_token": "x"}), "a", None).is_err());
        assert!(tokens_from_response(json!({"access_token": "x"}), "a", None).is_err());
    }

    #[test]
    fn stored_credentials_roundtrip() {
        let tokens = AuthTokens {
            username: "sora".into(),
            user_id: Some(7),
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        };
        let stored = tokens.to_stored();
        assert_eq!(stored.expires_at_unix, 1_800_000_000);
        assert_eq!(AuthTokens::from_stored(stored), tokens);
    }

    #[test]
    fn expired_tokens_are_detected() {
        let expired = AuthTokens {
            username: "s".into(),
            user_id: None,
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: UNIX_EPOCH,
        };
        assert!(expired.is_expired());
    }

    #[test]
    fn decodes_a_real_jwt_payload() {
        // {"anonymous":false,"username":"dream4u"}
        let token = "e30.eyJhbm9ueW1vdXMiOmZhbHNlLCJ1c2VybmFtZSI6ImRyZWFtNHUifQ.sig";
        let claims = jwt_claims(token).unwrap();
        assert_eq!(claims["username"], json!("dream4u"));
        assert_eq!(jwt_is_anonymous(token), Some(false));
        assert_eq!(jwt_claims("onlyonepart"), None);
        assert_eq!(jwt_is_anonymous(""), None);
    }

    #[tokio::test]
    async fn session_persists_on_demand() {
        let path = temp_path("persist");
        let tokens = AuthTokens {
            username: "sora".into(),
            user_id: Some(3),
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: UNIX_EPOCH + Duration::from_secs(1_900_000_000),
        };
        let session = AuthSession::new(tokens, Some(path.clone()));
        session.persist().await.unwrap();
        let stored = config::load_credentials_from(&path).unwrap().unwrap();
        assert_eq!(stored.username, "sora");
        assert_eq!(stored.refresh_token, "r");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn session_without_a_path_does_not_touch_the_disk() {
        let session = AuthSession::new(
            AuthTokens {
                username: "sora".into(),
                user_id: None,
                access_token: "a".into(),
                refresh_token: "r".into(),
                expires_at: UNIX_EPOCH,
            },
            None,
        );
        session.persist().await.unwrap();
    }
}
