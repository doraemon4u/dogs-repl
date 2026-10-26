//! Socket.IO transport for online-go.com (OGS).
//!
//! The wire conventions here are taken from `online-go/goban`'s `GobanSocket`,
//! which is OGS's own reference implementation of this protocol.
//!
//! Outbound — one JSON array per message:
//!
//! ```text
//! [command, data]        notify   (no reply expected)
//! [command]              notify   (no data)
//! [command, data, rid]   request  (reply carries the same rid)
//! ```
//!
//! Inbound — the first array element decides how to read it:
//!
//! ```text
//! [rid, data, error]     correlated reply to our request
//! [command, data]        unsolicited server push
//! ```
//!
//! Two things differ from goban and are deliberate:
//!
//! * We send `authenticate` as a request rather than a notify, so the server
//!   tells us whether the credential was accepted instead of leaving us to
//!   assume success. `user/update` is still accepted as confirmation.
//! * We must register `on("error", ..)` separately. `rust_socketio` folds the
//!   event name `"ERROR"` case-insensitively into `Event::Error`, which `on_any`
//!   never receives — without this, server errors are dropped silently.
//!
//! Transport note: OGS rejects engine.io polling with
//! `{"code":0,"message":"Transport unknown"}`, so WebSocket is mandatory.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use rust_socketio::asynchronous::{Client, ClientBuilder, ReconnectSettings};
use rust_socketio::{Event, Payload, TransportType};
use serde_json::{Value, json};
use tokio::sync::RwLock;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

const SOCKET_URL: &str = "https://online-go.com/socket.io/";
const PING_INTERVAL_SECS: u64 = 20;
const RECONNECT_DELAY_SECS: u64 = 5;
const MAX_RECONNECT_ATTEMPTS: u32 = 10;
const MAX_CONNECT_ATTEMPTS: u32 = 5;
const INITIAL_CONNECT_RETRY_SECS: u64 = 2;
/// How long a request may stay unanswered before it is failed and forgotten.
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// How long to wait for the server to acknowledge `authenticate`.
const AUTH_WATCHDOG_SECS: u64 = 12;

type Outbound = (String, Payload);

/// A normalised OGS event, delivered in order to a single consumer.
#[derive(Debug, Clone)]
pub enum RealtimeEvent {
    /// Socket handshake completed.
    Connected,
    /// Socket torn down.
    Disconnected,
    /// OGS accepted our credential.
    Authenticated { user_id: i64, username: String },
    /// `net/pong` — liveness confirmed.
    Heartbeat,
    /// `hostinfo` push.
    HostInfo(Value),
    /// `notification` push.
    Notification(Value),
    /// `active_game` push.
    ActiveGame(Value),
    /// `gamelist-count` push.
    GameListCount { list: String, count: u64 },
    /// A chat line from world or game chat.
    Chat {
        channel: String,
        author: String,
        text: String,
    },
    /// Any `game/<id>/...` push, with the full event name.
    GameEvent { event: String, data: Value },
    /// Correlated reply to a request, resolved through its rid.
    Response {
        rid: u64,
        command: String,
        data: Value,
        error: Option<Value>,
    },
    /// Transport or protocol failure.
    Error(String),
}

/// The `list` selector `gamelist/query` accepts; anything else is rejected.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum GameList {
    Live,
    Corr,
    Kidsgo,
}

impl GameList {
    fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Corr => "corr",
            Self::Kidsgo => "kidsgo",
        }
    }
}

struct Inner {
    jwt: String,
    tx: UnboundedSender<RealtimeEvent>,
    socket: Arc<RwLock<Option<Client>>>,
    next_rid: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, String>>>,
    /// Ordered outbound queue. `tokio::spawn`-per-message would allow a
    /// `game/move` to overtake the `authenticate` that authorises it.
    out_tx: UnboundedSender<Outbound>,
    out_rx: Mutex<Option<UnboundedReceiver<Outbound>>>,
    worker_started: AtomicBool,
    authenticated: AtomicBool,
    watchdog_started: AtomicBool,
}

impl Inner {
    fn new(jwt: String, tx: UnboundedSender<RealtimeEvent>) -> Self {
        let (out_tx, out_rx) = unbounded_channel();
        Self {
            jwt,
            tx,
            socket: Arc::new(RwLock::new(None)),
            next_rid: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            out_tx,
            out_rx: Mutex::new(Some(out_rx)),
            worker_started: AtomicBool::new(false),
            authenticated: AtomicBool::new(false),
            watchdog_started: AtomicBool::new(false),
        }
    }

    /// `[command, data, rid]` — the server answers with the same rid.
    fn request(&self, command: &str, data: Value) {
        let rid = self.next_rid.fetch_add(1, Ordering::Relaxed);
        self.pending
            .lock()
            .expect("pending map poisoned")
            .insert(rid, command.to_string());

        // Drop the pending entry and tell the user if the server never
        // answers. Without this a lost reply leaves the UI waiting forever.
        let pending = Arc::clone(&self.pending);
        let tx = self.tx.clone();
        let label = command.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS)).await;
            let still_pending = pending
                .lock()
                .expect("pending map poisoned")
                .remove(&rid)
                .is_some();
            if still_pending {
                let _ = tx.send(RealtimeEvent::Error(format!(
                    "{label} timed out after {REQUEST_TIMEOUT_SECS}s (request id {rid})"
                )));
            }
        });

        self.emit(command, Payload::Text(vec![data, Value::from(rid)]));
    }

    /// `[command, data]` — fire and forget, matching goban's no-callback send.
    fn notify(&self, command: &str, data: Value) {
        self.emit(command, Payload::Text(vec![data]));
    }

    fn is_authenticated(&self) -> bool {
        self.authenticated.load(Ordering::Relaxed)
    }

    /// Enqueues an outbound message. A single worker drains this queue in
    /// order, so request ordering is stable.
    fn emit(&self, command: &str, payload: Payload) {
        if tracing_enabled() {
            eprintln!("[trace] -> {command} {}", payload_label(&payload));
        }
        if self
            .out_tx
            .send((command.to_string(), payload))
            .is_err()
        {
            let _ = self.tx.send(RealtimeEvent::Error(format!(
                "cannot send `{command}`: outbound queue closed"
            )));
        }
    }

    /// Starts the ordered outbound worker once per client.
    fn start_outbound_worker(&self) {
        if self.worker_started.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(mut rx) = self.out_rx.lock().expect("out_rx poisoned").take() else {
            return;
        };
        let socket = Arc::clone(&self.socket);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            while let Some((command, payload)) = rx.recv().await {
                let guard = socket.read().await;
                let Some(client) = guard.as_ref() else {
                    let _ = tx.send(RealtimeEvent::Error(format!(
                        "cannot send `{command}`: socket not connected"
                    )));
                    continue;
                };
                if let Err(error) = client.emit(command.clone(), payload).await {
                    let _ = tx.send(RealtimeEvent::Error(format!(
                        "send `{command}` failed: {error}"
                    )));
                }
            }
        });
    }

    fn dispatch(&self, event: Event, payload: Payload) {
        if trace_everything() || (tracing_enabled() && is_auth_relevant(&event)) {
            eprintln!(
                "[trace] <- {} {}",
                event_label(&event),
                payload_label(&payload)
            );
        }

        let values = match payload {
            Payload::Text(values) => values,
            Payload::Binary(_) => return,
            #[allow(deprecated)]
            Payload::String(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(value) => vec![value],
                Err(_) => return,
            },
        };

        // A numeric first element means this is the answer to one of our
        // requests; anything else is a push event.
        let (event_name, data) = match values.first() {
            Some(Value::Number(first)) => {
                let rid = match first.as_u64() {
                    Some(rid) => rid,
                    None => return,
                };
                let command = self
                    .pending
                    .lock()
                    .expect("pending map poisoned")
                    .remove(&rid)
                    .unwrap_or_else(|| format!("#{rid}"));
                let data = values.get(1).cloned().unwrap_or(Value::Null);
                let error = values.get(2).cloned().filter(|value| !value.is_null());

                if command == "authenticate" && error.is_none() {
                    self.send_authenticated(&data);
                }
                if let Some(error) = &error {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .map_or_else(|| error.to_string(), str::to_string);
                    let _ = self.tx.send(RealtimeEvent::Error(format!(
                        "{command}: {message}"
                    )));
                }

                let _ = self.tx.send(RealtimeEvent::Response {
                    rid,
                    command,
                    data,
                    error,
                });
                return;
            }
            Some(Value::String(name)) if event.as_str() == "message" => {
                (name.as_str(), values.get(1).cloned().unwrap_or(Value::Null))
            }
            _ => (event.as_str(), values.first().cloned().unwrap_or(Value::Null)),
        };

        self.push_event(event_name, data);
    }

    fn push_event(&self, name: &str, data: Value) {
        let event = match name {
            "net/pong" => RealtimeEvent::Heartbeat,
            "hostinfo" => RealtimeEvent::HostInfo(data),
            "notification" => RealtimeEvent::Notification(data),
            "active_game" => RealtimeEvent::ActiveGame(data),
            "gamelist-count" => {
                let list = data
                    .get("list")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let count = data.get("count").and_then(Value::as_u64).unwrap_or(0);
                RealtimeEvent::GameListCount { list, count }
            }
            "user/update" => {
                self.send_authenticated(&data);
                return;
            }
            "chat" | "chat-message" => RealtimeEvent::Chat {
                channel: data
                    .get("channel")
                    .and_then(Value::as_str)
                    .unwrap_or("world")
                    .trim_start_matches('#')
                    .to_string(),
                author: data
                    .get("username")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                text: data
                    .get("text")
                    .or_else(|| data.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
            "private-message" | "private-superchat" => {
                let author = data
                    .get("from")
                    .and_then(|u| u.get("username"))
                    .and_then(Value::as_str)
                    .unwrap_or("private")
                    .to_string();
                let text = data
                    .get("message")
                    .and_then(|m| m.get("message").or_else(|| m.get("body")))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                RealtimeEvent::Chat {
                    channel: format!("pm:{}", author),
                    author,
                    text,
                }
            }
            name if name.ends_with("/chat") => RealtimeEvent::Chat {
                channel: name.trim_end_matches("/chat").to_string(),
                author: data
                    .get("username")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                text: data
                    .get("text")
                    .or_else(|| data.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            },
            name if name.starts_with("game/") => RealtimeEvent::GameEvent {
                event: name.to_string(),
                data,
            },
            _ => return,
        };
        let _ = self.tx.send(event);
    }

    fn send_authenticated(&self, data: &Value) {
        self.authenticated.store(true, Ordering::Relaxed);
        let user_id = data.get("id").and_then(Value::as_i64).unwrap_or(0);
        let username = data
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let _ = self.tx.send(RealtimeEvent::Authenticated { user_id, username });
    }

    /// Body of `authenticate`, per `protocol.ts`.
    fn auth_body(&self) -> Value {
        json!({
            "jwt": self.jwt,
            "client": "ogs-tui",
            "client_version": env!("CARGO_PKG_VERSION"),
            "user_agent": concat!("ogs-tui/", env!("CARGO_PKG_VERSION")),
        })
    }

    /// `net/ping`. `client` must be epoch milliseconds: the server subtracts it
    /// from its own clock to derive the drift it reports in `net/pong`.
    fn ping_body() -> Value {
        let client = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        json!({ "client": client, "drift": 0, "latency": 0 })
    }
}

/// Handle to a live OGS socket connection.
pub struct RealtimeClient {
    inner: Arc<Inner>,
    event_rx: Option<UnboundedReceiver<RealtimeEvent>>,
}

impl RealtimeClient {
    /// Creates a client that authenticates with `jwt`.
    ///
    /// `jwt` is the `user_jwt` field from `GET /api/v1/ui/config`, which is a
    /// different credential from the REST access token.
    pub fn new(jwt: String) -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            inner: Arc::new(Inner::new(jwt, tx)),
            event_rx: Some(rx),
        }
    }

    /// Transfers the inbound event stream out; `None` if already taken.
    pub fn take_event_receiver(&mut self) -> Option<UnboundedReceiver<RealtimeEvent>> {
        self.event_rx.take()
    }

    /// Whether the server has acknowledged our JWT.
    pub fn is_authenticated(&self) -> bool {
        self.inner.is_authenticated()
    }

    /// Opens the socket and authenticates.
    ///
    /// The engine.io handshake fails often enough that a single attempt
    /// strands the client with no socket, so this retries with backoff.
    /// `rust_socketio`'s own reconnect only covers a drop *after* connect.
    pub async fn connect(&self) {
        let mut delay = INITIAL_CONNECT_RETRY_SECS;
        for attempt in 1..=MAX_CONNECT_ATTEMPTS {
            match self.try_connect().await {
                Ok(()) => return,
                Err(message) => {
                    let _ = self.inner.tx.send(RealtimeEvent::Error(format!(
                        "connect attempt {attempt}/{MAX_CONNECT_ATTEMPTS} failed: {message}"
                    )));
                    if attempt < MAX_CONNECT_ATTEMPTS {
                        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                        delay = (delay * 2).min(RECONNECT_DELAY_SECS * 4);
                    }
                }
            }
        }
    }

    async fn try_connect(&self) -> Result<(), String> {
        self.inner.start_outbound_worker();
        self.inner.authenticated.store(false, Ordering::Relaxed);
        let any_inner = Arc::clone(&self.inner);
        let error_tx = self.inner.tx.clone();
        let reconnect_inner = Arc::clone(&self.inner);

        let builder = ClientBuilder::new(SOCKET_URL)
            .transport_type(TransportType::Websocket)
            .reconnect(true)
            .reconnect_delay(RECONNECT_DELAY_SECS, RECONNECT_DELAY_SECS * 2)
            .max_reconnect_attempts(MAX_RECONNECT_ATTEMPTS as u8)
            .on_any(move |event, payload, _client| {
                let inner = Arc::clone(&any_inner);
                async move {
                    inner.dispatch(event, payload);
                }
                .boxed()
            })
            // `on_any` only sees Event::Message and Event::Custom, and
            // `Event::from("ERROR")` is Event::Error, so server-reported errors
            // would never reach the dispatcher.
            .on("error", move |payload, _client| {
                let tx = error_tx.clone();
                async move {
                    let _ = tx.send(RealtimeEvent::Error(payload_text(&payload)));
                }
                .boxed()
            })
            .on_reconnect(move || {
                let inner = Arc::clone(&reconnect_inner);
                async move {
                    let mut settings = ReconnectSettings::new();
                    settings.address(SOCKET_URL);
                    settings.auth(json!({ "auth": { "jwt": inner.jwt } }));
                    // A reconnected socket is unauthenticated again.
                    inner.authenticated.store(false, Ordering::Relaxed);
                    let _ = inner.tx.send(RealtimeEvent::Connected);
                    if let Some(client) = inner.socket.read().await.as_ref() {
                        let _ = client
                            .emit(
                                "authenticate",
                                Payload::Text(vec![inner.auth_body(), Value::from(0)]),
                            )
                            .await;
                    }
                    settings
                }
                .boxed()
            });

        match builder.connect().await {
            Ok(client) => {
                *self.inner.socket.write().await = Some(client);
                let _ = self.inner.tx.send(RealtimeEvent::Connected);
                self.authenticate();
                self.spawn_heartbeat();
                self.spawn_auth_watchdog();
                Ok(())
            }
            // `IncompleteResponseFromEngineIo` renders as just "EngineIO Error"
            // and discards the cause, so unwrap it for the log.
            Err(rust_socketio::Error::IncompleteResponseFromEngineIo(inner)) => {
                Err(format!("{inner:?}"))
            }
            Err(other) => Err(other.to_string()),
        }
    }

    /// Closes the socket and drops unresolved requests.
    pub async fn disconnect(&self) {
        let taken = self.inner.socket.write().await.take();
        self.inner.pending.lock().expect("pending map poisoned").clear();
        if let Some(client) = taken {
            if let Err(e) = client.disconnect().await {
                let _ = self.inner.tx.send(RealtimeEvent::Error(format!(
                    "disconnect failed: {e}"
                )));
            }
        }
        let _ = self.inner.tx.send(RealtimeEvent::Disconnected);
    }

    /// Offers the credential to the server.
    pub fn authenticate(&self) {
        let body = self.inner.auth_body();
        self.inner.request("authenticate", body);
    }

    /// `net/ping` — OGS treats a silent socket as offline.
    pub fn ping(&self) {
        self.inner.notify("net/ping", Inner::ping_body());
    }

    /// `hostinfo` — server version and limits.
    pub fn host_info(&self) {
        self.inner.request("hostinfo", json!({}));
    }

    /// `gamelist/query` — one page of `list`, filtered by `filter`.
    pub fn query_game_list(&self, list: GameList, filter: Value, from: i64, limit: i64) {
        self.inner.request(
            "gamelist/query",
            json!({
                "list": list.as_str(),
                "sort_by": "rank",
                "where": filter,
                "from": from,
                "limit": limit,
            }),
        );
    }

    /// `game/connect` — subscribes to a game's push stream.
    pub fn connect_to_game(&self, game_id: i64) {
        self.inner.request("game/connect", json!({ "game_id": game_id }));
    }

    /// `game/disconnect` — stops the push stream for a game.
    pub fn disconnect_from_game(&self, game_id: i64) {
        self.inner.notify("game/disconnect", json!({ "game_id": game_id }));
    }

    /// `game/move` — submits a move in GTP coordinate form, e.g. `"D4"`.
    pub fn play_move(&self, game_id: i64, mv: &str) {
        self.inner
            .request("game/move", json!({ "game_id": game_id, "move": mv }));
    }

    /// `game/move` with the protocol's pass marker.
    pub fn pass(&self, game_id: i64) {
        self.inner
            .request("game/move", json!({ "game_id": game_id, "move": ".." }));
    }

    /// `game/resign` — concedes the game.
    pub fn resign(&self, game_id: i64) {
        self.inner.notify("game/resign", json!({ "game_id": game_id }));
    }

    /// `game/chat` — posts to a game's main chat, addressed to a move.
    pub fn game_chat(&self, game_id: i64, move_number: i64, body: &str) {
        self.inner.request(
            "game/chat",
            json!({
                "game_id": game_id,
                "type": "main",
                "move_number": move_number,
                "body": body,
            }),
        );
    }

    /// `game/undo/request` — asks the opponent for a take-back.
    pub fn request_undo(&self, game_id: i64, move_number: i64) {
        self.inner.request(
            "game/undo/request",
            json!({ "game_id": game_id, "move_number": move_number }),
        );
    }

    /// `chat/join` — subscribes to a world chat channel.
    pub fn chat_join(&self, channel: &str) {
        self.inner.notify("chat/join", json!({ "channel": channel }));
    }

    /// `chat/part` — leaves a world chat channel.
    pub fn chat_part(&self, channel: &str) {
        self.inner.notify("chat/part", json!({ "channel": channel }));
    }

    /// `chat/send` — posts to a world chat channel.
    pub fn send_chat(&self, channel: &str, message: &str) {
        let uuid = self.next_uuid();
        self.inner.notify(
            "chat/send",
            json!({ "channel": channel, "uuid": uuid, "message": message }),
        );
    }

    /// `chat/topic` — changes a channel topic.
    pub fn chat_topic(&self, channel: &str, topic: &str) {
        self.inner
            .request("chat/topic", json!({ "channel": channel, "topic": topic }));
    }

    /// `user/monitor` — subscribes to presence changes of these player ids.
    pub fn monitor_user(&self, player_ids: &[i64]) {
        self.inner.notify("user/monitor", json!({ "user_ids": player_ids }));
    }

    /// `net/route_latency` — pings every OGS front-end node.
    pub fn route_latency(&self) {
        self.inner.request("net/route_latency", json!({}));
    }

    fn next_uuid(&self) -> String {
        let uuid = self.inner.next_rid.fetch_add(1, Ordering::Relaxed);
        format!("ogs-tui-{uuid}")
    }

    /// Warns if the socket is up but the credential was never accepted. A
    /// pong alone proves connectivity, not authorisation.
    fn spawn_auth_watchdog(&self) {
        let inner = Arc::clone(&self.inner);
        if inner.watchdog_started.swap(true, Ordering::Relaxed) {
            return;
        }
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(AUTH_WATCHDOG_SECS)).await;
            if inner.socket.read().await.is_some() && !inner.is_authenticated() {
                let _ = inner.tx.send(RealtimeEvent::Error(format!(
                    "socket connected but authentication did not complete within {AUTH_WATCHDOG_SECS}s; \
                     check that /ui/config returned a user_jwt for the logged-in account"
                )));
            }
        });
    }

    fn spawn_heartbeat(&self) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(PING_INTERVAL_SECS));
            // The first tick resolves immediately; skip it so we do not ping the
            // instant after connecting.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if inner.socket.read().await.is_none() {
                    return;
                }
                // Go through the ordered queue so a heartbeat cannot jump in
                // front of an earlier request.
                inner.notify("net/ping", Inner::ping_body());
            }
        });
    }
}

/// The rest of the `ClientToServer` command surface from `protocol.ts`.
///
/// Kept separate from the wired commands above because several entries here
/// are not reachable from the REPL yet; they exist so the client covers the
/// protocol rather than only what the current UI happens to call.
#[allow(dead_code)]
impl RealtimeClient {
    /// `game/pause` — freezes both clocks.
    pub fn game_pause(&self, game_id: i64) {
        self.inner.notify("game/pause", json!({ "game_id": game_id }));
    }

    /// `game/resume` — restarts the clocks.
    pub fn game_resume(&self, game_id: i64) {
        self.inner.notify("game/resume", json!({ "game_id": game_id }));
    }

    /// `game/undo/accept` — agrees to a take-back request.
    pub fn game_undo_accept(&self, game_id: i64, move_number: i64) {
        self.inner.notify(
            "game/undo/accept",
            json!({ "game_id": game_id, "move_number": move_number }),
        );
    }

    /// `game/undo/cancel` — refuses a take-back request.
    pub fn game_undo_cancel(&self, game_id: i64, move_number: i64) {
        self.inner.notify(
            "game/undo/cancel",
            json!({ "game_id": game_id, "move_number": move_number }),
        );
    }

    /// `game/delayed_resign` — resigns, letting the opponent respond first.
    pub fn game_delayed_resign(&self, game_id: i64) {
        self.inner
            .notify("game/delayed_resign", json!({ "game_id": game_id }));
    }

    /// `game/clear_delayed_resign` — revokes a pending delayed resign.
    pub fn game_clear_delayed_resign(&self, game_id: i64) {
        self.inner
            .notify("game/clear_delayed_resign", json!({ "game_id": game_id }));
    }

    /// `game/cancel` — cancels the game before it starts.
    pub fn game_cancel(&self, game_id: i64) {
        self.inner.notify("game/cancel", json!({ "game_id": game_id }));
    }

    /// `game/annul` — voids the game.
    pub fn game_annul(&self, game_id: i64) {
        self.inner.notify("game/annul", json!({ "game_id": game_id }));
    }

    /// `game/timed_out` — reports the local client hit the move deadline.
    pub fn game_timed_out(&self, game_id: i64) {
        self.inner.notify("game/timed_out", json!({ "game_id": game_id }));
    }

    /// `game/prevent_stalling` — claims the game instead of playing it out.
    pub fn game_prevent_stalling(&self, game_id: i64, winner: &str) {
        self.inner.notify(
            "game/prevent_stalling",
            json!({ "game_id": game_id, "winner": winner }),
        );
    }

    /// `game/prevent_escaping` — claims the game over a stalling line.
    pub fn game_prevent_escaping(&self, game_id: i64, winner: &str, annul: bool) {
        self.inner.notify(
            "game/prevent_escaping",
            json!({ "game_id": game_id, "winner": winner, "annul": annul }),
        );
    }

    /// `game/removed_stones/set` — marks dead stones or dame during scoring.
    pub fn game_removed_stones_set(&self, game_id: i64, removed: bool, stones: &[String]) {
        self.inner.request(
            "game/removed_stones/set",
            json!({ "game_id": game_id, "removed": removed, "stones": stones }),
        );
    }

    /// `game/removed_stones/accept` — locks in the agreed scoring.
    pub fn game_removed_stones_accept(&self, game_id: i64, stones: &str) {
        self.inner.request(
            "game/removed_stones/accept",
            json!({
                "game_id": game_id,
                "stones": stones,
                "strict_seki_mode": false,
            }),
        );
    }

    /// `game/removed_stones/reject` — disputes the opponent's scoring.
    pub fn game_removed_stones_reject(&self, game_id: i64) {
        self.inner
            .request("game/removed_stones/reject", json!({ "game_id": game_id }));
    }

    /// `game/conditional_moves/set` — publishes the take-back tree.
    pub fn game_conditional_moves_set(&self, game_id: i64, move_number: i64, tree: Value) {
        self.inner.request(
            "game/conditional_moves/set",
            json!({
                "game_id": game_id,
                "move_number": move_number,
                "conditional_moves": tree,
            }),
        );
    }

    /// `game/latency` — reports measured latency to the opponent.
    pub fn game_latency(&self, game_id: i64, latency: i64) {
        self.inner.notify(
            "game/latency",
            json!({ "game_id": game_id, "latency": latency }),
        );
    }

    /// `game/log` — fetches the game's server-side event log.
    pub fn game_log(&self, game_id: i64) {
        self.inner.request("game/log", json!({ "game_id": game_id }));
    }

    /// `game/chat/remove` — removes one line of game chat.
    pub fn game_chat_remove(&self, game_id: i64, channel: &str, chat_id: &str) {
        self.inner.notify(
            "game/chat/remove",
            json!({ "game_id": game_id, "channel": channel, "chat_id": chat_id }),
        );
    }

    /// `chat/pm` — sends a private message.
    pub fn chat_pm(&self, player_id: i64, username: &str, uid: &str, message: &str) {
        self.inner.request(
            "chat/pm",
            json!({
                "player_id": player_id,
                "username": username,
                "uid": uid,
                "message": message,
            }),
        );
    }

    /// `chat/pm/load` — opens the private-message thread with a player.
    pub fn chat_pm_load(&self, player_id: i64) {
        self.inner.request("chat/pm/load", json!({ "player_id": player_id }));
    }

    /// `chat/pm/close` — closes that thread.
    pub fn chat_pm_close(&self, player_id: i64) {
        self.inner.notify("chat/pm/close", json!({ "player_id": player_id }));
    }

    /// `chat/pm/superchat` — toggles the moderator's unclosable modal.
    pub fn chat_pm_superchat(&self, player_id: i64, username: &str, enable: bool) {
        self.inner.request(
            "chat/pm/superchat",
            json!({ "player_id": player_id, "username": username, "enable": enable }),
        );
    }

    /// `chat/remove` — deletes one world chat line by uuid.
    pub fn chat_remove(&self, uuid: &str) {
        self.inner.notify("chat/remove", json!({ "uuid": uuid }));
    }

    /// `chat/remove_all` — deletes every message from a player.
    pub fn chat_remove_all(&self, player_id: i64) {
        self.inner.notify("chat/remove_all", json!({ "player_id": player_id }));
    }

    /// `review/connect` — subscribes to a finished game's review stream.
    pub fn review_connect(&self, review_id: i64) {
        self.inner
            .notify("review/connect", json!({ "review_id": review_id }));
    }

    /// `review/disconnect` — leaves a review stream.
    pub fn review_disconnect(&self, review_id: i64) {
        self.inner
            .notify("review/disconnect", json!({ "review_id": review_id }));
    }

    /// `review/append` — adds a node to the review analysis tree.
    pub fn review_append(&self, message: Value) {
        self.inner.notify("review/append", message);
    }

    /// `review/chat` — posts an analysis comment on a branch.
    pub fn review_chat(&self, review_id: i64, from: i64, moves: &str, body: &str) {
        self.inner.request(
            "review/chat",
            json!({
                "review_id": review_id,
                "from": from,
                "moves": moves,
                "body": body,
            }),
        );
    }

    /// `review/chat/remove` — removes one analysis comment.
    pub fn review_chat_remove(&self, review_id: i64, channel: &str, chat_id: &str) {
        self.inner.notify(
            "review/chat/remove",
            json!({ "review_id": review_id, "channel": channel, "chat_id": chat_id }),
        );
    }

    /// `notification/delete` — dismisses a notification.
    pub fn notification_delete(&self, notification_id: &str) {
        self.inner.notify("notification/delete", json!({ "notification_id": notification_id }));
    }

    /// `gamelist/count/subscribe` — streams game counts for `channel`.
    pub fn gamelist_count_subscribe(&self, channel: Option<&str>) {
        self.inner
            .notify("gamelist/count/subscribe", json!({ "channel": channel }));
    }

    /// `gamelist/count/unsubscribe` — stops that count stream.
    pub fn gamelist_count_unsubscribe(&self, channel: Option<&str>) {
        self.inner
            .notify("gamelist/count/unsubscribe", json!({ "channel": channel }));
    }

    /// `stats/online` — polls the online-user count every `interval` seconds.
    pub fn stats_online(&self, interval: i64) {
        self.inner
            .request("stats/online", json!({ "interval": interval }));
    }

    /// `challenge/keepalive` — keeps a live challenge from timing out.
    pub fn challenge_keepalive(&self, challenge_id: i64, game_id: i64) {
        self.inner.notify(
            "challenge/keepalive",
            json!({ "challenge_id": challenge_id, "game_id": game_id }),
        );
    }

    /// `net/connects` — server-side connection telemetry.
    pub fn net_connects(&self, route: &str, times_connected: i64, previous_ms: i64) {
        self.inner.notify(
            "net/connects",
            json!({
                "route": route,
                "times_connected": times_connected,
                "device_info": { "user_agent": "ogs-tui" },
                "previous_connection_duration_ms": previous_ms,
            }),
        );
    }
}

fn event_label(event: &Event) -> String {
    match event {
        Event::Message => "Message".to_string(),
        Event::Error => "Error".to_string(),
        other => format!("Custom({other})"),
    }
}

fn payload_label(payload: &Payload) -> String {
    match payload {
        Payload::Text(values) => format!("{values:?}"),
        #[allow(deprecated)]
        Payload::String(s) => format!("{s:?}"),
        Payload::Binary(b) => format!("<{} bytes>", b.len()),
    }
}

fn payload_text(payload: &Payload) -> String {
    match payload {
        Payload::Text(values) => values
            .first()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default(),
        #[allow(deprecated)]
        Payload::String(s) => s.clone(),
        Payload::Binary(_) => "<binary>".to_string(),
    }
}

/// Set `OGS_TUI_TRACE=0` to silence, `=all` to also dump the `active-bots`
/// catalog.
///
/// On by default: OGS pushes that catalog right after connect — over a
/// thousand frames — which buries the few frames that explain login. Outbound
/// traffic is low-volume and always shown; inbound is filtered to events that
/// matter for authentication.
fn tracing_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("OGS_TUI_TRACE").as_deref() != Ok("0"))
}

fn trace_everything() -> bool {
    static ALL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ALL.get_or_init(|| std::env::var("OGS_TUI_TRACE").as_deref() == Ok("all"))
}

fn is_auth_relevant(event: &Event) -> bool {
    match event {
        Event::Message | Event::Error => true,
        Event::Custom(name) => name != "active-bots",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inner() -> (Arc<Inner>, UnboundedReceiver<RealtimeEvent>) {
        let (tx, rx) = unbounded_channel();
        (Arc::new(Inner::new("jwt".to_string(), tx)), rx)
    }

    fn seed(inner: &Inner, rid: u64, command: &str) {
        inner
            .pending
            .lock()
            .unwrap()
            .insert(rid, command.to_string());
    }

    #[test]
    fn request_carries_the_rid_in_the_third_position() {
        // goban: `cb ? [command, data, ++id] : [command, data]`
        let payload = Payload::Text(vec![json!({"game_id": 5}), json!(9)]);
        let wire = Value::Array(
            std::iter::once(Value::String("game/move".to_string()))
                .chain(match payload {
                    Payload::Text(values) => values,
                    _ => unreachable!(),
                })
                .collect(),
        );
        assert_eq!(wire, json!(["game/move", {"game_id": 5}, 9]));
    }

    #[test]
    fn notify_omits_the_rid() {
        let payload = Payload::Text(vec![json!({"channel": "general"})]);
        let wire = Value::Array(
            std::iter::once(Value::String("chat/join".to_string()))
                .chain(match payload {
                    Payload::Text(values) => values,
                    _ => unreachable!(),
                })
                .collect(),
        );
        assert_eq!(wire, json!(["chat/join", {"channel": "general"}]));
    }

    #[test]
    fn rid_reply_resolves_to_the_originating_command() {
        let (inner, mut events) = inner();
        seed(&inner, 7, "gamelist/query");
        inner.dispatch(
            Event::Message,
            Payload::Text(vec![json!(7), json!({"a": 1}), Value::Null]),
        );
        match events.try_recv().unwrap() {
            RealtimeEvent::Response { rid, command, data, error } => {
                assert_eq!(rid, 7);
                assert_eq!(command, "gamelist/query");
                assert_eq!(data["a"], json!(1));
                assert!(error.is_none());
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_rid_resolves_only_once() {
        let (inner, mut events) = inner();
        seed(&inner, 7, "gamelist/query");
        inner.dispatch(Event::Message, Payload::Text(vec![json!(7), json!(1), Value::Null]));
        assert!(inner.pending.lock().unwrap().is_empty());
        let _ = events.try_recv();
        inner.dispatch(Event::Message, Payload::Text(vec![json!(7), json!(1), Value::Null]));
        match events.try_recv().unwrap() {
            RealtimeEvent::Response { command, .. } => assert_eq!(command, "#7"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn authenticate_reply_reports_the_user() {
        let (inner, mut events) = inner();
        seed(&inner, 1, "authenticate");
        inner.dispatch(
            Event::Message,
            Payload::Text(vec![json!(1), json!({"id": 42, "username": "sora"}), Value::Null]),
        );
        match events.try_recv().unwrap() {
            RealtimeEvent::Authenticated { user_id, username } => {
                assert_eq!(user_id, 42);
                assert_eq!(username, "sora");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn error_reply_surfaces_a_message() {
        let (inner, mut events) = inner();
        seed(&inner, 3, "game/move");
        inner.dispatch(
            Event::Message,
            Payload::Text(vec![
                json!(3),
                Value::Null,
                json!({"message": "not your turn"}),
            ]),
        );
        match events.try_recv().unwrap() {
            RealtimeEvent::Error(message) => assert_eq!(message, "game/move: not your turn"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn user_update_push_is_reported_as_authenticated() {
        let (inner, mut events) = inner();
        inner.dispatch(
            Event::from("user/update"),
            Payload::Text(vec![json!({"id": 77, "username": "dream4u"})]),
        );
        match events.try_recv().unwrap() {
            RealtimeEvent::Authenticated { user_id, username } => {
                assert_eq!((user_id, username), (77, "dream4u".to_string()));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn named_push_becomes_a_game_event() {
        let (inner, mut events) = inner();
        inner.dispatch(
            Event::from("game/1234/gamedata"),
            Payload::Text(vec![json!({"phase": "play"})]),
        );
        match events.try_recv().unwrap() {
            RealtimeEvent::GameEvent { event, data } => {
                assert_eq!(event, "game/1234/gamedata");
                assert_eq!(data["phase"], json!("play"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn game_chat_push_is_split_into_author_and_text() {
        let (inner, mut events) = inner();
        inner.dispatch(
            Event::from("game/1234/chat"),
            Payload::Text(vec![json!({"username": "sora", "text": "hi"})]),
        );
        match events.try_recv().unwrap() {
            RealtimeEvent::Chat { channel, author, text } => {
                assert_eq!((channel.as_str(), author.as_str(), text.as_str()),
                           ("game/1234", "sora", "hi"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn ogs_error_events_never_reach_on_any() {
        // `Event::from` lowercases, so OGS's ["ERROR", ...] becomes
        // Event::Error, which rust_socketio routes only to `on("error", ..)`.
        // If this ever starts failing, the `on_any` dispatch could take ERROR.
        assert_eq!(Event::from("ERROR"), Event::Error);
        assert_eq!(Event::from("error"), Event::Error);
        assert!(matches!(Event::from("hostinfo"), Event::Custom(_)));
    }

    #[test]
    fn ping_body_carries_epoch_milliseconds() {
        let body = Inner::ping_body();
        assert!(body.get("client").is_some());
        assert_eq!(body["drift"], json!(0));
        assert_eq!(body["latency"], json!(0));
        // A zero or seconds-scale timestamp would report wild clock drift.
        assert!(body["client"].as_u64().unwrap() > 1_600_000_000_000);
    }

    #[test]
    fn auth_body_sends_the_user_jwt_not_a_bare_token() {
        let (inner, _rx) = inner();
        let body = inner.auth_body();
        assert_eq!(body["jwt"], json!("jwt"));
        assert_eq!(body["client"], json!("ogs-tui"));
    }

    #[test]
    fn rids_are_unique_and_monotonic() {
        let (inner, _rx) = inner();
        let first = inner.next_rid.fetch_add(1, Ordering::Relaxed);
        let second = inner.next_rid.fetch_add(1, Ordering::Relaxed);
        assert!(second > first);
    }

    #[test]
    fn game_list_variants_match_the_protocol_literals() {
        assert_eq!(GameList::Live.as_str(), "live");
        assert_eq!(GameList::Corr.as_str(), "corr");
        assert_eq!(GameList::Kidsgo.as_str(), "kidsgo");
    }
}