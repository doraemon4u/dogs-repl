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
        if let Some(handlers) = handlers.get(event) {
            for handler in handlers {
                handler(event, data);
            }
        }
    }
}
