//! Realtime transport for online-go.com (OGS).
//!
//! OGS's realtime API is *not* Socket.IO: it is a plain WebSocket at
//! `wss://online-go.com` carrying one JSON array per message. This matches
//! `online-go/goban`'s `GobanSocket`, OGS's own reference client. An earlier
//! version spoke Socket.IO framing; the server still pushed chat through it,
//! but never answered requests, so `authenticate` (and with it chat sending)
//! silently failed.
//!
//! Outbound:
//!
//! ```text
//! [command, data]        notify   (no reply expected)
//! [command, data, rid]   request  (reply carries the same rid)
//! ```
//!
//! Inbound — the first array element decides how to read it:
//!
//! ```text
//! [rid, data, error]     reply to our request
//! [command, data]        server push (["ERROR", text] is a server error)
//! ```
//!
//! On every (re)connect `authenticate` is written first, before anything that
//! was queued while offline, exactly like goban's open handler.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, header};
use tokio_tungstenite::tungstenite::Message;

const SOCKET_URL: &str = "wss://online-go.com";
const PING_INTERVAL_SECS: u64 = 20;
/// First retry delay after a failed or dropped connection; doubles up to
/// [`MAX_RETRY_SECS`].
const INITIAL_CONNECT_RETRY_SECS: u64 = 2;
const MAX_RETRY_SECS: u64 = 30;
/// Give up on a TCP/TLS/WebSocket handshake after this long.
const CONNECT_TIMEOUT_SECS: u64 = 15;
/// How long a request may stay unanswered before it is failed and forgotten.
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// How long to wait for the server to acknowledge `authenticate`.
const AUTH_WATCHDOG_SECS: u64 = 12;
/// How long a sent chat line may stay unconfirmed before the user is told.
const CHAT_CONFIRM_SECS: u64 = 10;

/// One queued frame: the command, its arguments, and for requests the id the
/// reply will carry.
type Outbound = (String, Payload, Option<u64>);

/// Name of an inbound frame.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    /// A reply (`[rid, data, error]`).
    Message,
    /// `["ERROR", …]`.
    Error,
    Custom(String),
}

impl Event {
    fn as_str(&self) -> &str {
        match self {
            Self::Message => "message",
            Self::Error => "error",
            Self::Custom(name) => name,
        }
    }
}

impl From<&str> for Event {
    fn from(name: &str) -> Self {
        if name.eq_ignore_ascii_case("error") {
            Self::Error
        } else if name == "message" {
            Self::Message
        } else {
            Self::Custom(name.to_string())
        }
    }
}

impl std::fmt::Display for Event {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The values after the command name in a frame.
#[derive(Debug, Clone, PartialEq)]
enum Payload {
    Text(Vec<Value>),
}

/// Serialises `[command, …values, rid?]`.
fn encode_frame(command: &str, payload: Payload, rid: Option<u64>) -> String {
    let Payload::Text(values) = payload;
    let mut frame = Vec::with_capacity(values.len() + 2);
    frame.push(Value::from(command));
    frame.extend(values);
    if let Some(rid) = rid {
        frame.push(Value::from(rid));
    }
    Value::Array(frame).to_string()
}

/// OGS's `n2s`: a number in base 62, digits `0-9A-Za-z`.
fn base62(mut n: u64) -> String {
    const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut digits = Vec::new();
    while n > 0 {
        digits.push(DIGITS[(n % 62) as usize]);
        n /= 62;
    }
    digits.reverse();
    String::from_utf8(digits).expect("base-62 digits are ASCII")
}

/// A normalised OGS event, delivered in order to a single consumer.
#[derive(Debug, Clone)]
pub enum RealtimeEvent {
    /// The online-bot catalog (`active-bots`), keyed by player id.
    ActiveBots(Value),
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
    /// Users joined or present in a chat channel (`chat-join` / `chat-update-user`).
    ChatJoin {
        channel: String,
        users: Vec<(i64, String)>,
        history: Vec<Value>,
    },
    /// Channel chat history sync (`chat-history` / `chat/history`).
    ChatHistory {
        channel: String,
        messages: Vec<Value>,
    },
    /// A user left a chat channel (`chat-part`).
    ChatPart {
        channel: String,
        user_id: i64,
        username: String,
    },
    /// Channel topic updated (`chat-topic`).
    ChatTopic {
        channel: String,
        topic: String,
    },
    /// A chat message was removed (`chat-message-removed`).
    ChatRemoved {
        channel: String,
        uuid: String,
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
    /// Writer for the live connection; `None` while offline. Frames queued
    /// while offline wait here, like goban's `send_queue`.
    link: tokio::sync::watch::Sender<Option<UnboundedSender<String>>>,
    /// Set by `disconnect`; stops reconnecting.
    closed: AtomicBool,
    shutdown: tokio::sync::Notify,
    /// Bumped on every connection, so per-connection timers can tell
    /// whether they still belong to the current socket.
    generation: AtomicU64,
    next_rid: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, String>>>,
    seen_chat_ids: Mutex<std::collections::HashSet<String>>,
    /// Ordered outbound queue. `tokio::spawn`-per-message would allow a
    /// `game/move` to overtake the `authenticate` that authorises it.
    out_tx: UnboundedSender<Outbound>,
    out_rx: Mutex<Option<UnboundedReceiver<Outbound>>>,
    worker_started: AtomicBool,
    authenticated: AtomicBool,
    /// How many times the server accepted our JWT on this client.
    auth_count: AtomicU64,
    /// The player id the server authenticated us as; the first part of
    /// every chat message id.
    user_id: AtomicI64,
    /// Epoch milliseconds of the last chat message id, kept strictly
    /// increasing.
    last_chat_stamp: Mutex<u64>,
    /// Wire channels we are subscribed to, replayed after a reconnect.
    joined_channels: Mutex<std::collections::BTreeSet<String>>,
    /// `chat/send` requests the server has not answered yet:
    /// request id → wire channel.
    pending_chat: Arc<Mutex<HashMap<u64, String>>>,
}

impl Inner {
    fn new(jwt: String, tx: UnboundedSender<RealtimeEvent>) -> Self {
        let (out_tx, out_rx) = unbounded_channel();
        Self {
            jwt,
            tx,
            link: tokio::sync::watch::channel(None).0,
            closed: AtomicBool::new(false),
            shutdown: tokio::sync::Notify::new(),
            generation: AtomicU64::new(0),
            next_rid: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            seen_chat_ids: Mutex::new(std::collections::HashSet::new()),
            out_tx,
            out_rx: Mutex::new(Some(out_rx)),
            worker_started: AtomicBool::new(false),
            authenticated: AtomicBool::new(false),
            auth_count: AtomicU64::new(0),
            user_id: AtomicI64::new(0),
            last_chat_stamp: Mutex::new(0),
            joined_channels: Mutex::new(std::collections::BTreeSet::new()),
            pending_chat: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Re-joins every chat channel after the server accepts a reconnect's
    /// JWT. (The session re-opens the active game itself.)
    fn replay_subscriptions(&self) {
        let channels: Vec<String> = self
            .joined_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect();
        for channel in channels {
            self.notify("chat/join", json!({ "channel": channel }));
        }
    }

    /// A chat message id in the form OGS's own client makes them
    /// (`chatSoftUid`): base-62 player id, a dot, base-62 epoch milliseconds.
    ///
    /// Ids must stay unique across sessions. OGS keeps them in the channel
    /// backlog, and clients (this one and the web client) silently drop a
    /// line whose id they have already seen.
    fn next_chat_uuid(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        let stamp = {
            let mut last = self
                .last_chat_stamp
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *last = now.max(*last + 1);
            *last
        };
        let user_id = self.user_id.load(Ordering::Relaxed);
        let user = if user_id < 0 {
            format!("-{}", base62(user_id.unsigned_abs()))
        } else {
            base62(user_id.unsigned_abs())
        };
        format!("{user}.{}", base62(stamp))
    }

    /// `[command, data, rid]` — the server answers with the same rid.
    fn request(&self, command: &str, data: Value) {
        // goban sends these without a callback and nothing here reads their
        // replies, so they stay notifications. (OGS does answer any frame
        // that carries an id; `send_chat` relies on that.)
        if command == "chat/join" || command == "chat/part" || command == "game/disconnect" {
            self.notify(command, data);
            return;
        }

        let rid = self.register_request(command);
        self.enqueue(command, Payload::Text(vec![data]), Some(rid));
    }

    /// Allocates a request id, remembers which command it answers, and
    /// reports an error if no reply arrives in time.
    fn register_request(&self, command: &str) -> u64 {
        let rid = self.track_request(command);
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
        rid
    }

    /// Allocates a request id and remembers which command it answers. The
    /// caller owns the timeout.
    fn track_request(&self, command: &str) -> u64 {
        let rid = self.next_rid.fetch_add(1, Ordering::Relaxed);
        self.pending
            .lock()
            .expect("pending map poisoned")
            .insert(rid, command.to_string());
        rid
    }

    fn connected(&self) -> bool {
        self.link.borrow().is_some()
    }

    /// `[command, data]` — fire and forget, matching goban's no-callback send.
    fn notify(&self, command: &str, data: Value) {
        self.enqueue(command, Payload::Text(vec![data]), None);
    }

    fn is_authenticated(&self) -> bool {
        self.authenticated.load(Ordering::Relaxed)
    }

    /// Enqueues an outbound message. A single worker drains this queue in
    /// order, so request ordering is stable.
    fn enqueue(&self, command: &str, payload: Payload, rid: Option<u64>) {
        if tracing_enabled() {
            let id = rid.map(|rid| format!(" (ack {rid})")).unwrap_or_default();
            eprintln!("[trace] -> {command} {}{id}", payload_label(&payload));
        }
        if self
            .out_tx
            .send((command.to_string(), payload, rid))
            .is_err()
        {
            let _ = self.tx.send(RealtimeEvent::Error(format!(
                "cannot send `{command}`: outbound queue closed"
            )));
        }
    }

    /// Starts the ordered outbound worker once per client. Frames wait until
    /// a connection exists, then go out in order.
    fn start_outbound_worker(self: &Arc<Self>) {
        if self.worker_started.swap(true, Ordering::Relaxed) {
            return;
        }
        let Some(mut rx) = self.out_rx.lock().expect("out_rx poisoned").take() else {
            return;
        };
        let inner = Arc::clone(self);
        tokio::spawn(async move {
            let mut link = inner.link.subscribe();
            while let Some((command, payload, rid)) = rx.recv().await {
                let frame = encode_frame(&command, payload, rid);
                loop {
                    let writer = link.borrow_and_update().clone();
                    if let Some(writer) = writer {
                        if writer.send(frame.clone()).is_ok() {
                            break;
                        }
                    }
                    if inner.closed.load(Ordering::Relaxed) || link.changed().await.is_err() {
                        return;
                    }
                }
            }
        });
    }

    /// Decodes one inbound WebSocket text message.
    fn handle_frame(&self, text: &str) {
        let Ok(Value::Array(mut values)) = serde_json::from_str::<Value>(text) else {
            return;
        };
        match values.first() {
            Some(Value::Number(_)) => self.dispatch(Event::Message, Payload::Text(values)),
            Some(Value::String(name)) => {
                let event = Event::from(name.as_str());
                let data = if values.len() > 1 {
                    values.swap_remove(1)
                } else {
                    Value::Null
                };
                if event == Event::Error {
                    if tracing_enabled() {
                        eprintln!("[trace] <- ERROR {data}");
                    }
                    let message = match &data {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    let _ = self.tx.send(RealtimeEvent::Error(format!("server: {message}")));
                    return;
                }
                self.dispatch(event, Payload::Text(vec![data]));
            }
            _ => {}
        }
    }

    fn dispatch(&self, event: Event, payload: Payload) {
        if trace_everything() || (tracing_enabled() && is_auth_relevant(&event)) {
            eprintln!(
                "[trace] <- {} {}",
                event_label(&event),
                payload_label(&payload)
            );
        }

        let Payload::Text(values) = payload;

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
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&rid)
                    .unwrap_or_else(|| format!("#{rid}"));
                let data = values.get(1).cloned().unwrap_or(Value::Null);
                let error = values
                    .get(2)
                    .cloned()
                    .filter(|value| !value.is_null())
                    .or_else(|| data.get("error").cloned().filter(|value| !value.is_null()));

                if command == "authenticate" && error.is_none() {
                    self.send_authenticated(&data);
                }
                // A chat line is confirmed by this reply, never by an echo:
                // OGS does not send a line back to the socket that sent it.
                let chat_channel = if command == "chat/send" {
                    self.pending_chat
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&rid)
                } else {
                    None
                };
                if let Some(error) = &error {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| error.as_str())
                        .map_or_else(|| error.to_string(), str::to_string);
                    let text = match &chat_channel {
                        Some(wire) => format!("OGS refused the message to {wire}: {message}"),
                        None => format!("{command}: {message}"),
                    };
                    let _ = self.tx.send(RealtimeEvent::Error(text));
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
            "active-bots" => RealtimeEvent::ActiveBots(data),
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
            "chat" | "chat-message" => {
                if let Some(msg_id) = crate::session::store::extract_chat_id(&data) {
                    let mut seen = self
                        .seen_chat_ids
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if !seen.insert(msg_id) {
                        return;
                    }
                    if seen.len() > 4000 {
                        seen.clear();
                    }
                }
                let raw_channel = data
                    .get("channel")
                    .and_then(Value::as_str)
                    .unwrap_or("en");
                let channel = crate::session::store::canonical_channel_key(raw_channel);
                let author = crate::session::store::extract_chat_author(&data);
                let Some(text) = crate::session::store::extract_chat_text(&data) else {
                    return;
                };
                RealtimeEvent::Chat {
                    channel,
                    author,
                    text,
                }
            }
            "chat-join" | "chat/join" | "channel-join" | "channel/join" => {
                let raw_channel = data
                    .get("channel")
                    .and_then(Value::as_str)
                    .unwrap_or("en");
                let channel = crate::session::store::canonical_channel_key(raw_channel);
                let users = data
                    .get("users")
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|u| {
                                let id = u.get("id").and_then(Value::as_i64).unwrap_or(0);
                                let name = u
                                    .get("username")
                                    .or_else(|| u.get("name"))
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string();
                                (!name.is_empty()).then_some((id, name))
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let history = data
                    .get("history")
                    .or_else(|| data.get("messages"))
                    .or_else(|| data.get("recent"))
                    .or_else(|| data.get("chat"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                RealtimeEvent::ChatJoin {
                    channel,
                    users,
                    history,
                }
            }
            "chat-history" | "channel-history" | "chat/history" | "channel/history" => {
                let raw_channel = data
                    .get("channel")
                    .and_then(Value::as_str)
                    .unwrap_or("en");
                let channel = crate::session::store::canonical_channel_key(raw_channel);
                let messages = if let Some(arr) = data.as_array() {
                    arr.clone()
                } else {
                    data.get("messages")
                        .or_else(|| data.get("history"))
                        .or_else(|| data.get("lines"))
                        .or_else(|| data.get("chat"))
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                };
                RealtimeEvent::ChatHistory { channel, messages }
            }
            "chat-update-user" => {
                let raw_channel = data
                    .get("channel")
                    .and_then(Value::as_str)
                    .unwrap_or("en");
                let channel = crate::session::store::canonical_channel_key(raw_channel);
                let users = data
                    .get("user")
                    .and_then(|u| {
                        let id = u.get("id").and_then(Value::as_i64).unwrap_or(0);
                        let name = u
                            .get("username")
                            .or_else(|| u.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        (!name.is_empty()).then_some(vec![(id, name)])
                    })
                    .unwrap_or_default();
                RealtimeEvent::ChatJoin {
                    channel,
                    users,
                    history: Vec::new(),
                }
            }
            "chat-part" => {
                let raw_channel = data
                    .get("channel")
                    .and_then(Value::as_str)
                    .unwrap_or("en");
                let channel = crate::session::store::canonical_channel_key(raw_channel);
                let user = data.get("user").unwrap_or(&data);
                let user_id = user.get("id").and_then(Value::as_i64).unwrap_or(0);
                let username = user
                    .get("username")
                    .or_else(|| user.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                RealtimeEvent::ChatPart {
                    channel,
                    user_id,
                    username,
                }
            }
            "chat-topic" => {
                let raw_channel = data
                    .get("channel")
                    .and_then(Value::as_str)
                    .unwrap_or("en");
                let channel = crate::session::store::canonical_channel_key(raw_channel);
                let topic = data
                    .get("topic")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                RealtimeEvent::ChatTopic { channel, topic }
            }
            "chat-message-removed" => {
                let raw_channel = data
                    .get("channel")
                    .and_then(Value::as_str)
                    .unwrap_or("en");
                let channel = crate::session::store::canonical_channel_key(raw_channel);
                let uuid = data
                    .get("uuid")
                    .or_else(|| data.get("chat_id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                RealtimeEvent::ChatRemoved { channel, uuid }
            }
            "private-message" | "private-superchat" => {
                let author = {
                    let extracted = crate::session::store::extract_chat_author(&data);
                    if extracted.is_empty() {
                        "private".to_string()
                    } else {
                        extracted
                    }
                };
                let Some(text) = crate::session::store::extract_chat_text(&data) else {
                    return;
                };
                RealtimeEvent::Chat {
                    channel: format!("pm:{}", author),
                    author,
                    text,
                }
            }
            name if name.ends_with("/chat") => {
                if let Some(msg_id) = crate::session::store::extract_chat_id(&data) {
                    let mut seen = self
                        .seen_chat_ids
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if !seen.insert(msg_id) {
                        return;
                    }
                    if seen.len() > 4000 {
                        seen.clear();
                    }
                }
                let author = crate::session::store::extract_chat_author(&data);
                let Some(text) = crate::session::store::extract_chat_text(&data) else {
                    return;
                };
                RealtimeEvent::Chat {
                    channel: name.trim_end_matches("/chat").to_string(),
                    author,
                    text,
                }
            }
            name if name.starts_with("game/") => RealtimeEvent::GameEvent {
                event: name.to_string(),
                data,
            },
            _ => return,
        };
        let _ = self.tx.send(event);
    }

    fn send_authenticated(&self, data: &Value) {
        let first = !self.authenticated.swap(true, Ordering::Relaxed);
        let user_id = data.get("id").and_then(Value::as_i64).unwrap_or(0);
        let username = data
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if user_id != 0 {
            self.user_id.store(user_id, Ordering::Relaxed);
        }
        // After a reconnect the server has forgotten our subscriptions.
        if first && self.auth_count.fetch_add(1, Ordering::Relaxed) > 0 {
            self.replay_subscriptions();
        }
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

    /// Opens the socket and keeps it open, reconnecting with backoff after
    /// any failure or drop until [`disconnect`](Self::disconnect) is called.
    pub async fn connect(&self) {
        self.inner.start_outbound_worker();
        self.spawn_heartbeat();
        let mut delay = INITIAL_CONNECT_RETRY_SECS;
        let mut failures = 0u32;
        while !self.inner.closed.load(Ordering::Relaxed) {
            match self.run_connection().await {
                Ok(()) => {
                    failures = 0;
                    delay = INITIAL_CONNECT_RETRY_SECS;
                }
                Err(message) => {
                    failures += 1;
                    let _ = self.inner.tx.send(RealtimeEvent::Error(format!(
                        "connect attempt {failures} failed: {message}; retrying in {delay}s"
                    )));
                }
            }
            if self.inner.closed.load(Ordering::Relaxed) {
                break;
            }
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(delay)) => {}
                _ = self.inner.shutdown.notified() => break,
            }
            if failures > 0 {
                delay = (delay * 2).min(MAX_RETRY_SECS);
            }
        }
    }

    /// One connection: handshake, authenticate, then read until it closes.
    /// `Ok` means it was connected and later dropped; `Err` means it never
    /// connected.
    async fn run_connection(&self) -> Result<(), String> {
        let inner = &self.inner;
        let mut request = SOCKET_URL
            .into_client_request()
            .map_err(|error| error.to_string())?;
        request.headers_mut().insert(
            header::USER_AGENT,
            HeaderValue::from_static(crate::transport::auth::USER_AGENT),
        );
        let (stream, _response) = tokio::time::timeout(
            std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS),
            tokio_tungstenite::connect_async(request),
        )
        .await
        .map_err(|_| format!("no answer from {SOCKET_URL} within {CONNECT_TIMEOUT_SECS}s"))?
        .map_err(|error| error.to_string())?;
        let (mut sink, mut source) = stream.split();

        let (writer, mut frames) = unbounded_channel::<String>();
        let write_task = tokio::spawn(async move {
            while let Some(text) = frames.recv().await {
                if sink.send(Message::Text(text)).await.is_err() {
                    break;
                }
            }
            let _ = sink.close().await;
        });

        inner.authenticated.store(false, Ordering::Relaxed);
        let generation = inner.generation.fetch_add(1, Ordering::Relaxed) + 1;
        // `authenticate` first, ahead of anything queued while offline.
        let rid = inner.register_request("authenticate");
        if tracing_enabled() {
            eprintln!("[trace] -> authenticate <credential> (rid {rid})");
        }
        let _ = writer.send(encode_frame(
            "authenticate",
            Payload::Text(vec![inner.auth_body()]),
            Some(rid),
        ));
        inner.link.send_replace(Some(writer));
        let _ = inner.tx.send(RealtimeEvent::Connected);
        self.spawn_auth_watchdog(generation);

        loop {
            if inner.closed.load(Ordering::Relaxed) {
                break;
            }
            tokio::select! {
                message = source.next() => match message {
                    Some(Ok(Message::Text(text))) => inner.handle_frame(&text),
                    Some(Ok(Message::Close(frame))) => {
                        if !inner.closed.load(Ordering::Relaxed) {
                            let reason = frame
                                .map(|frame| format!("{} {}", frame.code, frame.reason))
                                .unwrap_or_default();
                            let _ = inner.tx.send(RealtimeEvent::Error(format!(
                                "server closed the socket {reason}"
                            )));
                        }
                        break;
                    }
                    // Ping/pong is answered by tungstenite; OGS sends no binary.
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        let _ = inner
                            .tx
                            .send(RealtimeEvent::Error(format!("socket read failed: {error}")));
                        break;
                    }
                    None => break,
                },
                _ = inner.shutdown.notified() => break,
            }
        }

        inner.link.send_replace(None);
        inner.authenticated.store(false, Ordering::Relaxed);
        // Let the writer flush a close frame, then stop it.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), write_task).await;
        let _ = inner.tx.send(RealtimeEvent::Disconnected);
        Ok(())
    }

    /// Closes the socket for good and drops unresolved requests.
    pub async fn disconnect(&self) {
        self.inner.closed.store(true, Ordering::Relaxed);
        self.inner.pending.lock().expect("pending map poisoned").clear();
        let was_connected = self.inner.link.send_replace(None).is_some();
        self.inner.shutdown.notify_waiters();
        if !was_connected {
            let _ = self.inner.tx.send(RealtimeEvent::Disconnected);
        }
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

    /// `chat/join` — subscribes to a world chat channel. OGS answers with
    /// `chat-join` (who is there) and the recent backlog as `chat-message`
    /// pushes. Sent without a request id, like goban.
    ///
    /// Only the OGS wire name (e.g. `global-english`) is sent: join, part,
    /// and send must all use the same room id.
    pub fn chat_join(&self, channel: &str) {
        let wire = crate::session::store::to_ogs_wire_channel(channel);
        if wire.is_empty() {
            return;
        }
        self.inner
            .joined_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(wire.clone());
        self.inner.notify("chat/join", json!({ "channel": wire }));
    }

    /// `chat/part` — leaves a world chat channel.
    pub fn chat_part(&self, channel: &str) {
        let wire = crate::session::store::to_ogs_wire_channel(channel);
        self.inner
            .joined_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&wire);
        self.inner.notify("chat/part", json!({ "channel": wire }));
    }

    /// Wire channels currently subscribed.
    #[cfg(test)]
    pub fn joined_channels(&self) -> Vec<String> {
        self.inner
            .joined_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    /// `chat/send` — posts to a world chat channel and returns its uuid.
    ///
    /// OGS delivers the line to everyone else in the room but never sends
    /// it back to this socket, so the caller shows it locally. Instead the
    /// frame carries a request id: OGS answers `[rid, null, null]` once it
    /// has accepted the line and `[rid, null, error]` if it refused it.
    /// Silence for [`CHAT_CONFIRM_SECS`] is reported.
    pub fn send_chat(&self, channel: &str, message: &str) -> String {
        let wire = crate::session::store::to_ogs_wire_channel(channel);
        let needs_join = !self
            .inner
            .joined_channels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&wire);
        if needs_join {
            self.chat_join(&wire);
        }
        let uuid = self.inner.next_chat_uuid();
        // The line is on screen already; when it comes back in the backlog
        // after a rejoin, it must not be shown twice.
        self.inner
            .seen_chat_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(uuid.clone());
        let rid = self.inner.track_request("chat/send");
        self.inner
            .pending_chat
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(rid, wire.clone());
        self.inner.enqueue(
            "chat/send",
            Payload::Text(vec![
                json!({ "channel": wire, "uuid": uuid, "message": message }),
            ]),
            Some(rid),
        );

        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(CHAT_CONFIRM_SECS)).await;
            let unanswered = inner
                .pending_chat
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&rid);
            if let Some(wire) = unanswered {
                inner
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&rid);
                let _ = inner.tx.send(RealtimeEvent::Error(format!(
                    "chat to {wire} was not confirmed by OGS within {CHAT_CONFIRM_SECS}s \
                     (no reply; the connection may have dropped), so it may not have been delivered"
                )));
            }
        });
        uuid
    }

    /// `chat/topic` — changes a channel topic.
    pub fn chat_topic(&self, channel: &str, topic: &str) {
        let wire = crate::session::store::to_ogs_wire_channel(channel);
        self.inner
            .request("chat/topic", json!({ "channel": wire, "topic": topic }));
    }

    /// `user/monitor` — subscribes to presence changes of these player ids.
    pub fn monitor_user(&self, player_ids: &[i64]) {
        self.inner.notify("user/monitor", json!({ "user_ids": player_ids }));
    }

    /// `net/route_latency` — pings every OGS front-end node.
    pub fn route_latency(&self) {
        self.inner.request("net/route_latency", json!({}));
    }

    /// Warns if this connection is up but the credential was never accepted.
    /// A pong alone proves connectivity, not authorisation.
    fn spawn_auth_watchdog(&self, generation: u64) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(AUTH_WATCHDOG_SECS)).await;
            let current = inner.generation.load(Ordering::Relaxed) == generation;
            if current && inner.connected() && !inner.is_authenticated() {
                let _ = inner.tx.send(RealtimeEvent::Error(format!(
                    "socket connected but authentication did not complete within {AUTH_WATCHDOG_SECS}s; \
                     check that /ui/config returned a user_jwt for the logged-in account"
                )));
            }
        });
    }

    /// `net/ping` every [`PING_INTERVAL_SECS`] while connected; OGS treats
    /// a silent socket as offline.
    fn spawn_heartbeat(&self) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_secs(PING_INTERVAL_SECS));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if inner.closed.load(Ordering::Relaxed) {
                    return;
                }
                if inner.connected() {
                    inner.notify("net/ping", Inner::ping_body());
                }
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
        Event::Message => "reply".to_string(),
        Event::Error => "ERROR".to_string(),
        Event::Custom(name) => name.clone(),
    }
}

fn payload_label(payload: &Payload) -> String {
    let Payload::Text(values) = payload;
    format!("{values:?}")
}

/// Set `OGS_TUI_TRACE=1` (or `=all` to also dump the `active-bots` catalog)
/// or pass `--trace` on the command line to print socket frames to stderr.
///
/// Off by default so stderr output never corrupts the full-screen TUI buffer.
fn tracing_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        matches!(
            std::env::var("OGS_TUI_TRACE").as_deref(),
            Ok("1" | "true" | "TRUE" | "yes" | "YES" | "all" | "ALL")
        )
    })
}

fn trace_everything() -> bool {
    static ALL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ALL.get_or_init(|| {
        matches!(
            std::env::var("OGS_TUI_TRACE").as_deref(),
            Ok("all" | "ALL")
        )
    })
}

fn is_auth_relevant(event: &Event) -> bool {
    match event {
        Event::Message | Event::Error => true,
        Event::Custom(name) => name != "active-bots",
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

    /// A client with no socket whose outbound frames can be read back.
    fn client() -> (RealtimeClient, UnboundedReceiver<RealtimeEvent>, UnboundedReceiver<Outbound>) {
        let mut client = RealtimeClient::new("jwt".to_string());
        let events = client.take_event_receiver().unwrap();
        let outbound = client.inner.out_rx.lock().unwrap().take().unwrap();
        (client, events, outbound)
    }

    fn drain(outbound: &mut UnboundedReceiver<Outbound>) -> Vec<(String, Value)> {
        let mut frames = Vec::new();
        while let Ok((command, payload, _rid)) = outbound.try_recv() {
            let Payload::Text(values) = payload;
            let data = values.into_iter().next().unwrap_or(Value::Null);
            frames.push((command, data));
        }
        frames
    }

    #[tokio::test]
    async fn world_chat_joins_the_same_wire_room_before_sending() {
        let (client, _events, mut outbound) = client();
        let uuid = client.send_chat("en", "hello");
        let frames = drain(&mut outbound);
        assert_eq!(frames.len(), 2, "{frames:?}");
        assert_eq!(frames[0].0, "chat/join");
        assert_eq!(frames[0].1["channel"], "global-english");
        assert_eq!(frames[1].0, "chat/send");
        assert_eq!(frames[1].1["channel"], "global-english");
        assert_eq!(frames[1].1["uuid"], uuid);
        assert_eq!(frames[1].1["message"], "hello");

        // Already joined: no second join.
        client.send_chat("#EN", "again");
        let frames = drain(&mut outbound);
        assert_eq!(frames.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["chat/send"]);
    }

    #[test]
    fn join_and_part_use_only_the_wire_name() {
        let (client, _events, mut outbound) = client();
        client.chat_join("zh");
        client.chat_part("zh");
        let frames = drain(&mut outbound);
        assert_eq!(frames.len(), 2, "one frame each, no raw-name duplicate: {frames:?}");
        assert!(frames.iter().all(|(_, data)| data["channel"] == "global-zh-hans"));
        assert!(client.joined_channels().is_empty());
    }

    /// The next queued frame with its request id.
    fn next_frame(outbound: &mut UnboundedReceiver<Outbound>) -> (String, Value, Option<u64>) {
        let (command, payload, rid) = outbound.try_recv().expect("a queued frame");
        let Payload::Text(values) = payload;
        (command, values.into_iter().next().unwrap_or(Value::Null), rid)
    }

    #[tokio::test]
    async fn chat_send_is_confirmed_by_the_servers_reply_not_an_echo() {
        // Seen live: OGS answers `["chat/send", {...}, 5]` with `[5,null,null]`
        // and never sends the line back to the sending socket.
        let (client, mut events, mut outbound) = client();
        client.chat_join("en");
        let (command, _, rid) = next_frame(&mut outbound);
        assert_eq!((command.as_str(), rid), ("chat/join", None));

        client.send_chat("en", "hello");
        let (command, data, rid) = next_frame(&mut outbound);
        assert_eq!(command, "chat/send");
        assert_eq!(data["message"], "hello");
        let rid = rid.expect("chat/send carries a request id");

        client.inner.handle_frame(&format!("[{rid},null,null]"));
        assert!(client.inner.pending_chat.lock().unwrap().is_empty());
        assert!(client.inner.pending.lock().unwrap().is_empty());
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(event, RealtimeEvent::Error(_)), "{event:?}");
        }

        // A refusal names the channel and the server's reason.
        client.send_chat("en", "again");
        let (_, _, rid) = next_frame(&mut outbound);
        let rid = rid.unwrap();
        client
            .inner
            .handle_frame(&format!(r#"[{rid},null,{{"message":"you are muted"}}]"#));
        let errors: Vec<String> = std::iter::from_fn(|| events.try_recv().ok())
            .filter_map(|event| match event {
                RealtimeEvent::Error(message) => Some(message),
                _ => None,
            })
            .collect();
        assert_eq!(
            errors,
            ["OGS refused the message to global-english: you are muted"]
        );
        assert!(client.inner.pending_chat.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn our_line_in_a_later_backlog_is_not_shown_twice_but_others_are() {
        let (client, mut events, _outbound) = client();
        client.inner.send_authenticated(&json!({"id": 1, "username": "me"}));
        let _ = events.try_recv();
        let uuid = client.send_chat("en", "hi all");

        // After a rejoin OGS replays the backlog, our line included.
        client.inner.dispatch(
            Event::from("chat-message"),
            Payload::Text(vec![json!({
                "channel": "global-english", "id": 1, "username": "me",
                "message": {"i": uuid, "m": "hi all", "t": 1}
            })]),
        );
        assert!(events.try_recv().is_err(), "already shown locally");

        // The same account posting from another device is a new line, and
        // so is somebody else saying the same thing.
        for (id, user, msg_id) in [(1, "me", "8vF9.web"), (2, "you", "x.1")] {
            client.inner.dispatch(
                Event::from("chat-message"),
                Payload::Text(vec![json!({
                    "channel": "global-english", "id": id, "username": user,
                    "message": {"i": msg_id, "m": "hi all", "t": 2}
                })]),
            );
            assert!(matches!(events.try_recv(), Ok(RealtimeEvent::Chat { .. })));
        }
    }

    #[tokio::test]
    async fn chat_ids_follow_ogs_soft_uid_and_never_repeat() {
        assert_eq!(base62(0), "0");
        assert_eq!(base62(61), "z");
        assert_eq!(base62(62), "10");
        assert_eq!(base62(2_126_671), "8vF9");

        let (client, mut events, _outbound) = client();
        client
            .inner
            .send_authenticated(&json!({"id": 2_126_671, "username": "dream4u"}));
        let _ = events.try_recv();
        let first = client.send_chat("zh", "a");
        let second = client.send_chat("zh", "b");
        assert!(first.starts_with("8vF9."), "{first}");
        assert!(second.starts_with("8vF9."), "{second}");
        assert_ne!(first, second);
        // Anchored to the clock rather than a per-process counter, so a
        // restart cannot reuse an id that is still in the channel backlog.
        let decode = |digits: &str| {
            digits.chars().fold(0u64, |n, c| {
                let digit = match c {
                    '0'..='9' => c as u64 - '0' as u64,
                    'A'..='Z' => c as u64 - 'A' as u64 + 10,
                    _ => c as u64 - 'a' as u64 + 36,
                };
                n * 62 + digit
            })
        };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let stamp = decode(first.trim_start_matches("8vF9."));
        assert!(now_ms.abs_diff(stamp) < 60_000, "{first} is not epoch ms");
        assert!(decode(second.trim_start_matches("8vF9.")) > stamp);
    }

    #[tokio::test]
    async fn channels_are_rejoined_only_after_a_reconnect_authenticates() {
        let (client, mut events, mut outbound) = client();
        client.chat_join("en");
        client.chat_join("ja");
        drain(&mut outbound);

        // First authentication: nothing to replay.
        client.inner.send_authenticated(&json!({"id": 1, "username": "me"}));
        assert!(drain(&mut outbound).is_empty());

        // Reconnect: the new socket is unauthenticated until the server
        // accepts the JWT again, and only then are channels rejoined.
        client.inner.authenticated.store(false, Ordering::Relaxed);
        assert!(drain(&mut outbound).is_empty());
        client.inner.send_authenticated(&json!({"id": 1, "username": "me"}));
        let rejoined: Vec<_> = drain(&mut outbound)
            .into_iter()
            .map(|(command, data)| (command, data["channel"].as_str().unwrap().to_string()))
            .collect();
        assert_eq!(
            rejoined,
            [
                ("chat/join".to_string(), "global-english".to_string()),
                ("chat/join".to_string(), "global-japanese".to_string()),
            ]
        );
        while events.try_recv().is_ok() {}
    }

    #[test]
    fn frames_are_plain_json_arrays_like_goban() {
        assert_eq!(
            encode_frame("chat/join", Payload::Text(vec![json!({"channel": "global-english"})]), None),
            r#"["chat/join",{"channel":"global-english"}]"#
        );
        assert_eq!(
            encode_frame("authenticate", Payload::Text(vec![json!({"jwt": "x"})]), Some(3)),
            r#"["authenticate",{"jwt":"x"},3]"#
        );
    }

    #[test]
    fn inbound_frames_are_split_into_replies_pushes_and_errors() {
        let (inner, mut events) = inner();
        seed(&inner, 1, "authenticate");
        // What OGS answers to `["authenticate", {...}, 1]` (seen live).
        inner.handle_frame(r#"[1,{"id":2126671,"username":"dream4u"},null]"#);
        match events.try_recv().unwrap() {
            RealtimeEvent::Authenticated { user_id, username } => {
                assert_eq!((user_id, username.as_str()), (2126671, "dream4u"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert!(inner.is_authenticated());
        let _ = events.try_recv(); // the Response event

        inner.handle_frame(r#"["chat-message",{"channel":"global-english","id":5,"username":"sora","message":{"i":"a1","m":"hi","t":1}}]"#);
        assert!(matches!(events.try_recv(), Ok(RealtimeEvent::Chat { .. })));

        inner.handle_frame(r#"["ERROR","Invalid jwt"]"#);
        match events.try_recv().unwrap() {
            RealtimeEvent::Error(message) => assert_eq!(message, "server: Invalid jwt"),
            other => panic!("unexpected: {other:?}"),
        }

        // Garbage and non-array frames are ignored.
        inner.handle_frame("not json");
        inner.handle_frame(r#"{"x":1}"#);
        assert!(events.try_recv().is_err());
    }

    #[tokio::test]
    async fn queued_frames_wait_for_a_connection_then_go_out_in_order() {
        let mut client = RealtimeClient::new("jwt".to_string());
        let _events = client.take_event_receiver().unwrap();
        client.inner.start_outbound_worker();
        client.chat_join("en");
        client.inner.notify("net/ping", json!({}));
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let (writer, mut wire) = unbounded_channel::<String>();
        client.inner.link.send_replace(Some(writer));
        let first = tokio::time::timeout(std::time::Duration::from_secs(1), wire.recv())
            .await
            .unwrap()
            .unwrap();
        let second = wire.recv().await.unwrap();
        assert_eq!(first, r#"["chat/join",{"channel":"global-english"}]"#);
        assert!(second.starts_with(r#"["net/ping""#));
        client.disconnect().await;
    }

    #[test]
    fn request_carries_the_rid_in_the_third_position() {
        // goban: `cb ? [command, data, ++id] : [command, data]`
        let payload = Payload::Text(vec![json!({"game_id": 5}), json!(9)]);
        let wire = Value::Array(
            std::iter::once(Value::String("game/move".to_string()))
                .chain({
                    let Payload::Text(values) = payload;
                    values
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
                .chain({
                    let Payload::Text(values) = payload;
                    values
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
    fn error_frames_are_recognised_case_insensitively() {
        // OGS sends ["ERROR", text]; it must not be mistaken for a push event.
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

    #[test]
    fn ogs_nested_chat_message_and_channel_events_are_dispatched() {
        let (inner, mut events) = inner();

        // 1. OGS `chat-message` with nested `message: { i, t, m }` on `global-zh-hans`
        inner.dispatch(
            Event::from("chat-message"),
            Payload::Text(vec![json!({
                "channel": "global-zh-hans",
                "user": { "id": 7, "username": "weiqi_fan", "ranking": 30 },
                "message": {
                    "i": "uuid-abc",
                    "t": 1_729_000_000,
                    "m": "有人来一盘 19 路吗？"
                }
            })]),
        );
        match events.try_recv().unwrap() {
            RealtimeEvent::Chat {
                channel,
                author,
                text,
            } => {
                assert_eq!(channel, "zh");
                assert_eq!(author, "weiqi_fan");
                assert_eq!(text, "有人来一盘 19 路吗？");
            }
            other => panic!("unexpected: {other:?}"),
        }

        // 2. OGS `chat-join`
        inner.dispatch(
            Event::from("chat-join"),
            Payload::Text(vec![json!({
                "channel": "global-english",
                "users": [{ "id": 1, "username": "nomad" }]
            })]),
        );
        match events.try_recv().unwrap() {
            RealtimeEvent::ChatJoin {
                channel,
                users,
                history,
            } => {
                assert_eq!(channel, "en");
                assert_eq!(users, vec![(1, "nomad".to_string())]);
                assert!(history.is_empty());
            }
            other => panic!("unexpected: {other:?}"),
        }

        // 3. OGS `chat-topic`
        inner.dispatch(
            Event::from("chat-topic"),
            Payload::Text(vec![json!({
                "channel": "global-english",
                "topic": "Welcome to OGS English Channel"
            })]),
        );
        match events.try_recv().unwrap() {
            RealtimeEvent::ChatTopic { channel, topic } => {
                assert_eq!(channel, "en");
                assert_eq!(topic, "Welcome to OGS English Channel");
            }
            other => panic!("unexpected: {other:?}"),
        }

        // 4. OGS `game/:id/chat` with nested `line: { username, body }`
        inner.dispatch(
            Event::from("game/42/chat"),
            Payload::Text(vec![json!({
                "channel": "main",
                "line": {
                    "chat_id": "line-1",
                    "username": "alice",
                    "body": "gg thanks!"
                }
            })]),
        );
        match events.try_recv().unwrap() {
            RealtimeEvent::Chat {
                channel,
                author,
                text,
            } => {
                assert_eq!(channel, "game/42");
                assert_eq!(author, "alice");
                assert_eq!(text, "gg thanks!");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}