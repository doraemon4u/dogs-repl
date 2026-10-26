//! Event bus for distributing Realtime events to subscribers.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub type EventHandler = Arc<dyn Fn(&str, &serde_json::Value) + Send + Sync>;

#[derive(Default)]
pub struct EventBus {
    handlers: RwLock<HashMap<String, Vec<EventHandler>>>,
}

impl EventBus {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            handlers: RwLock::new(HashMap::new()),
        })
    }

    pub async fn subscribe(&self, event: &str, handler: EventHandler) {
        let mut handlers = self.handlers.write().await;
        handlers.entry(event.to_string()).or_default().push(handler);
    }

    pub async fn publish(&self, event: &str, data: &serde_json::Value) {
        let handlers = self.handlers.read().await;
        if let Some(exact) = handlers.get(event) {
            for handler in exact {
                handler(event, data);
            }
        }
        if event != "*" {
            if let Some(wildcard) = handlers.get("*") {
                for handler in wildcard {
                    handler(event, data);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn publishes_to_exact_and_wildcard_subscribers() {
        let bus = EventBus::new();
        let exact_hits = Arc::new(AtomicUsize::new(0));
        let wildcard_hits = Arc::new(AtomicUsize::new(0));

        let exact_clone = Arc::clone(&exact_hits);
        bus.subscribe(
            "game/42/move",
            Arc::new(move |event, data| {
                assert_eq!(event, "game/42/move");
                assert_eq!(data.get("x").and_then(|v| v.as_i64()), Some(3));
                exact_clone.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .await;

        let wildcard_clone = Arc::clone(&wildcard_hits);
        bus.subscribe(
            "*",
            Arc::new(move |_event, _data| {
                wildcard_clone.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .await;

        bus.publish("game/42/move", &json!({"x": 3})).await;
        bus.publish("chat-message", &json!({"text": "hi"})).await;

        assert_eq!(exact_hits.load(Ordering::SeqCst), 1);
        assert_eq!(wildcard_hits.load(Ordering::SeqCst), 2);
    }
}

