//! REST API client for OGS Django endpoints.
//!
//! Every path here was verified against the live server: an unauthenticated
//! probe returns 200/401/403 for endpoints that exist and 404 for ones that do
//! not. Note that `players/{username}/` does **not** resolve — the detail route
//! takes a numeric id, so lookups go through the `players/?username=` filter.
//!
//! All requests go through [`RestClient::request`], which refreshes the OAuth
//! token on 401 and backs off on 429/5xx instead of hammering a rate-limited
//! endpoint.

use crate::transport::auth::{self, AuthSession};
use serde_json::{Value, json};
use std::time::Duration;

const BASE_URL: &str = "https://online-go.com/api/v1";
const MAX_RETRIES: u32 = 3;
const BASE_BACKOFF_MS: u64 = 500;
/// Cap on honoring `Retry-After`, so a bogus header cannot hang the client.
const MAX_RETRY_AFTER_SECS: u64 = 10;

pub struct RestClient {
    http: reqwest::Client,
    base: String,
    auth: AuthSession,
}

impl RestClient {
    /// Creates a client for the public OGS API.
    pub fn new(auth: AuthSession) -> Self {
        Self {
            http: auth::http_client().clone(),
            base: BASE_URL.to_string(),
            auth,
        }
    }

    /// Creates a client against an alternate base (tests, mock servers).
    pub fn with_base(auth: AuthSession, base: impl Into<String>) -> Self {
        Self {
            http: auth::http_client().clone(),
            base: base.into(),
            auth,
        }
    }

    pub fn auth(&self) -> &AuthSession {
        &self.auth
    }

    /// `GET /me` — the authenticated user.
    pub async fn me(&self) -> Result<Value, String> {
        self.get("/me").await
    }

    /// `GET /me/games?ended__isnull=true` — games the user is still in.
    pub async fn my_active_games(&self) -> Result<Value, String> {
        self.get("/me/games?ended__isnull=true").await
    }

    /// `GET /me/challenges` — challenges addressed to the user.
    pub async fn my_challenges(&self) -> Result<Value, String> {
        self.get("/me/challenges").await
    }

    /// `GET /ui/config` — public server config.
    ///
    /// Carries the `user_jwt` that the realtime `authenticate` command needs;
    /// it is a different credential from the REST access token.
    pub async fn ui_config(&self) -> Result<Value, String> {
        self.get("/ui/config").await
    }

    /// `GET /ui/overview` — the lobby snapshot.
    pub async fn overview(&self) -> Result<Value, String> {
        self.get("/ui/overview").await
    }

    /// `GET /games/{id}/` — one game.
    pub async fn game(&self, game_id: &str) -> Result<Value, String> {
        self.get(&format!("/games/{}/", game_id)).await
    }

    /// `GET /games/{id}/sgf` — raw SGF text.
    pub async fn game_sgf(&self, game_id: &str) -> Result<String, String> {
        self.get_text(&format!("/games/{}/sgf", game_id)).await
    }

    /// `GET /challenges/` — open challenges.
    pub async fn challenges(&self) -> Result<Value, String> {
        self.get("/challenges/").await
    }

    /// `POST /challenges/` — issues a challenge.
    pub async fn create_challenge(&self, options: &ChallengeOptions) -> Result<Value, String> {
        self.post("/challenges/", &options.to_body()).await
    }

    /// `POST /challenges/{id}/accept/`
    pub async fn accept_challenge(&self, challenge_id: &str) -> Result<Value, String> {
        self.post(
            &format!("/challenges/{}/accept/", challenge_id),
            &Value::Null,
        )
        .await
    }

    /// `POST /challenges/{id}/decline/`
    pub async fn decline_challenge(&self, challenge_id: &str) -> Result<Value, String> {
        self.post(
            &format!("/challenges/{}/decline/", challenge_id),
            &Value::Null,
        )
        .await
    }

    /// `GET /tournaments/` — tournament list.
    pub async fn tournaments(&self) -> Result<Value, String> {
        self.get("/tournaments/").await
    }

    /// `GET /tournaments/{id}/` — one tournament.
    pub async fn tournament(&self, tournament_id: i64) -> Result<Value, String> {
        self.get(&format!("/tournaments/{}/", tournament_id)).await
    }

    /// `GET /puzzles/` — puzzle collection.
    pub async fn puzzles(&self) -> Result<Value, String> {
        self.get("/puzzles/").await
    }

    /// `GET /puzzles/{id}/` — one puzzle.
    pub async fn puzzle(&self, puzzle_id: i64) -> Result<Value, String> {
        self.get(&format!("/puzzles/{}/", puzzle_id)).await
    }

    /// `GET /players/?username=` — resolves a username to a player record.
    ///
    /// The detail route is id-keyed, so this filter is the only way in by name.
    pub async fn player_by_username(&self, username: &str) -> Result<Value, String> {
        let page = self
            .get(&format!("/players/?username={}", urlencode(username)))
            .await?;
        page_results(&page)
            .into_iter()
            .next()
            .ok_or_else(|| format!("no player named {username}"))
    }

    /// `GET /players/{id}/` — one player by numeric id.
    pub async fn player(&self, player_id: i64) -> Result<Value, String> {
        self.get(&format!("/players/{}/", player_id)).await
    }

    /// `GET /chat/channels/` — the language/world channels.
    ///
    /// The list is fetched at runtime rather than hard-coded, so channels that
    /// OGS adds or retires do not make this client stale. Some server versions
    /// return the list embedded in `/ui/config` instead; callers should fall
    /// back to [`Self::ui_config`].
    pub async fn chat_channels(&self) -> Result<Value, String> {
        self.get("/chat/channels/").await
    }

    /// `GET /players/?username_contains=` — search players by name.
    pub async fn search_players(&self, query: &str) -> Result<Value, String> {
        let page = self
            .get(&format!(
                "/players/?username__icontains={}",
                urlencode(query)
            ))
            .await?;
        Ok(Value::Array(page_results(&page)))
    }

    pub async fn get(&self, path: &str) -> Result<Value, String> {
        self.request(reqwest::Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        self.request(reqwest::Method::POST, path, Some(body)).await
    }

    pub async fn get_text(&self, path: &str) -> Result<String, String> {
        let mut refreshed = false;
        let mut attempt = 0u32;
        loop {
            self.auth.ensure_valid().await?;
            let token = self.auth.access_token().await;
            let response = self
                .http
                .get(self.url(path))
                .bearer_auth(&token)
                .header("Accept", "application/x-go-sgf, text/plain")
                .send()
                .await
                .map_err(|error| format!("request failed: {error}"))?;

            let status = response.status();
            if status.is_success() {
                return response
                    .text()
                    .await
                    .map_err(|error| format!("read failed: {error}"));
            }
            if status == reqwest::StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                self.auth.refresh_after_unauthorized().await?;
                continue;
            }
            if is_retryable(status) && attempt < MAX_RETRIES {
                let wait = retry_after(&response).unwrap_or_else(|| backoff_delay(attempt));
                attempt += 1;
                tokio::time::sleep(wait).await;
                continue;
            }
            let body = response.text().await.unwrap_or_default();
            return Err(format_http_error(status, &body));
        }
    }

    /// Core request path with token refresh and retry/backoff.
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let mut refreshed = false;
        let mut attempt = 0u32;
        loop {
            self.auth.ensure_valid().await?;
            let token = self.auth.access_token().await;
            let mut request = self
                .http
                .request(method.clone(), self.url(path))
                .bearer_auth(&token)
                .header("Accept", "application/json")
                .header("Content-Type", "application/json");
            if let Some(body) = body {
                request = request.body(body.to_string());
            }
            let response = request
                .send()
                .await
                .map_err(|error| format!("request failed: {error}"))?;
            let status = response.status();

            if status.is_success() {
                let text = response
                    .text()
                    .await
                    .map_err(|error| format!("read failed: {error}"))?;
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                return serde_json::from_str(&text)
                    .map_err(|error| format!("parse failed: {error}"));
            }

            if status == reqwest::StatusCode::UNAUTHORIZED && !refreshed {
                refreshed = true;
                self.auth.refresh_after_unauthorized().await?;
                continue;
            }

            if is_retryable(status) && attempt < MAX_RETRIES {
                let wait = retry_after(&response).unwrap_or_else(|| backoff_delay(attempt));
                attempt += 1;
                tokio::time::sleep(wait).await;
                continue;
            }

            let body = response.text().await.unwrap_or_default();
            return Err(format_http_error(status, &body));
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }
}

fn format_http_error(status: reqwest::StatusCode, body: &str) -> String {
    let trimmed = body.trim();
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.is_empty() || lower.starts_with("<!doctype") || lower.starts_with("<html") {
        format!("HTTP {status}")
    } else {
        let single_line: String = trimmed
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let capped: String = single_line.chars().take(120).collect();
        format!("HTTP {status}: {capped}")
    }
}

/// Status codes worth retrying. A 429 is OGS rate limiting; 5xx is transient.
pub fn is_retryable(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

/// Exponential backoff with a hard cap, without jitter so it is testable.
pub fn backoff_delay(attempt: u32) -> Duration {
    let factor = 1u64 << attempt.min(5);
    Duration::from_millis((BASE_BACKOFF_MS * factor).min(8_000))
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|seconds| Duration::from_secs(seconds.min(MAX_RETRY_AFTER_SECS)))
}

/// Time control requested for a new challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengeClock {
    Byoyomi {
        main_time: u32,
        period_time: u32,
        periods: u32,
    },
    Fischer {
        initial_time: u32,
        increment: u32,
    },
    Absolute {
        main_time: u32,
    },
}

impl Default for ChallengeClock {
    fn default() -> Self {
        Self::Byoyomi {
            main_time: 600,
            period_time: 30,
            periods: 5,
        }
    }
}

impl ChallengeClock {
    fn apply(&self, body: &mut serde_json::Map<String, Value>) {
        match self {
            Self::Byoyomi {
                main_time,
                period_time,
                periods,
            } => {
                body.insert("time_control".into(), json!("byoyomi"));
                body.insert("main_time".into(), json!(main_time));
                body.insert("period_time".into(), json!(period_time));
                body.insert("periods".into(), json!(periods));
            }
            Self::Fischer {
                initial_time,
                increment,
            } => {
                body.insert("time_control".into(), json!("fischer"));
                body.insert("initial_time".into(), json!(initial_time));
                body.insert("time_increment".into(), json!(increment));
            }
            Self::Absolute { main_time } => {
                body.insert("time_control".into(), json!("absolute"));
                body.insert("main_time".into(), json!(main_time));
            }
        }
    }
}

/// Parameters for `POST /challenges/`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChallengeOptions {
    pub opponent: String,
    pub opponent_id: Option<i64>,
    pub board_size: u8,
    pub rules: String,
    pub ranked: bool,
    pub handicap: i32,
    pub komi: f64,
    /// `black`, `white`, or `automatic`.
    pub challenger_color: String,
    pub clock: ChallengeClock,
}

impl Default for ChallengeOptions {
    fn default() -> Self {
        Self {
            opponent: String::new(),
            opponent_id: None,
            board_size: 19,
            rules: "chinese".to_string(),
            ranked: true,
            handicap: 0,
            komi: 6.5,
            challenger_color: "automatic".to_string(),
            clock: ChallengeClock::default(),
        }
    }
}

impl ChallengeOptions {
    /// Builds the JSON body sent to `/api/v1/challenges/`.
    pub fn to_body(&self) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("challenge_type".into(), json!("personal"));
        body.insert("name".into(), json!(format!("ogs-tui vs {}", self.opponent)));
        body.insert("rules".into(), json!(self.rules));
        body.insert("board_size".into(), json!(self.board_size));
        body.insert("ranked".into(), json!(self.ranked));
        body.insert("handicap".into(), json!(self.handicap));
        body.insert("komi".into(), json!(self.komi));
        body.insert("challenger_color".into(), json!(self.challenger_color));
        body.insert("rengo".into(), json!(false));
        body.insert("min_rating".into(), json!(0));
        body.insert("max_rating".into(), json!(9999));
        if let Some(id) = self.opponent_id {
            body.insert("challenged_player_id".into(), json!(id));
        }
        self.clock.apply(&mut body);
        Value::Object(body)
    }
}

/// Unwraps the `{count, next, previous, results}` envelope OGS returns for
/// every list endpoint.
pub fn page_results(page: &Value) -> Vec<Value> {
    page.get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Percent-encodes the characters that would otherwise break a query string.
pub fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{:02X}", b),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::auth::AuthTokens;
    use std::time::{Duration as StdDuration, SystemTime};

    fn session() -> AuthSession {
        AuthSession::new(
            AuthTokens {
                username: "test".into(),
                user_id: None,
                access_token: "token".into(),
                refresh_token: "refresh".into(),
                expires_at: SystemTime::now() + StdDuration::from_secs(3600),
            },
            None,
        )
    }

    #[test]
    fn page_results_unwraps_the_drf_envelope() {
        let page = serde_json::json!({
            "count": 2,
            "next": "https://online-go.com/api/v1/players/?page=2",
            "previous": null,
            "results": [{"id": 4, "username": "matburt"}],
        });
        let results = page_results(&page);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["username"], "matburt");
    }

    #[test]
    fn page_results_tolerates_a_missing_results_key() {
        assert!(page_results(&serde_json::json!({})).is_empty());
        assert!(page_results(&serde_json::json!({"results": null})).is_empty());
    }

    #[test]
    fn urlencode_escapes_query_breaking_characters() {
        assert_eq!(urlencode("matburt"), "matburt");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
        assert_eq!(urlencode("q?#"), "q%3F%23");
    }

    #[test]
    fn urlencode_handles_multibyte_usernames() {
        // OGS usernames are CJK; each UTF-8 byte must be escaped individually.
        assert_eq!(urlencode("嘟嘟"), "%E5%98%9F%E5%98%9F");
    }

    #[test]
    fn retry_classification_and_backoff() {
        assert!(is_retryable(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!is_retryable(reqwest::StatusCode::NOT_FOUND));
        assert!(!is_retryable(reqwest::StatusCode::UNAUTHORIZED));
        assert_eq!(backoff_delay(0), Duration::from_millis(500));
        assert_eq!(backoff_delay(1), Duration::from_millis(1000));
        assert_eq!(backoff_delay(2), Duration::from_millis(2000));
        assert_eq!(backoff_delay(10), Duration::from_millis(8000));
    }

    #[test]
    fn challenge_body_has_the_personal_challenge_fields() {
        let options = ChallengeOptions {
            opponent: "kata".into(),
            opponent_id: Some(1234),
            board_size: 9,
            rules: "japanese".into(),
            ranked: false,
            komi: 5.5,
            clock: ChallengeClock::Fischer {
                initial_time: 300,
                increment: 5,
            },
            ..ChallengeOptions::default()
        };
        let body = options.to_body();
        assert_eq!(body["challenged_player_id"], serde_json::json!(1234));
        assert_eq!(body["board_size"], serde_json::json!(9));
        assert_eq!(body["rules"], serde_json::json!("japanese"));
        assert_eq!(body["ranked"], serde_json::json!(false));
        assert_eq!(body["komi"], serde_json::json!(5.5));
        assert_eq!(body["time_control"], serde_json::json!("fischer"));
        assert_eq!(body["initial_time"], serde_json::json!(300));
        assert_eq!(body["time_increment"], serde_json::json!(5));
        assert_eq!(body["challenger_color"], serde_json::json!("automatic"));
    }

    #[test]
    fn byoyomi_challenge_body_defaults() {
        let body = ChallengeOptions::default().to_body();
        assert_eq!(body["time_control"], serde_json::json!("byoyomi"));
        assert_eq!(body["main_time"], serde_json::json!(600));
        assert_eq!(body["period_time"], serde_json::json!(30));
        assert_eq!(body["periods"], serde_json::json!(5));
        assert!(body.get("challenged_player_id").is_none());
    }

    #[test]
    fn client_construction_does_not_touch_the_network() {
        let client = RestClient::with_base(session(), "http://127.0.0.1:9/api/v1");
        assert_eq!(client.url("/me"), "http://127.0.0.1:9/api/v1/me");
    }
}
