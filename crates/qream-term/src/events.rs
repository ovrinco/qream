use chrono::Local;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Topic {
    Pane,
    Agent,
    Session,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub v: u8,
    pub event: String,
    pub at: String,
    pub data: serde_json::Value,
}
impl Event {
    pub fn topic(&self) -> Option<Topic> {
        match self.event.split('.').next()? {
            "pane" => Some(Topic::Pane),
            "agent" => Some(Topic::Agent),
            "session" => Some(Topic::Session),
            _ => None,
        }
    }
}
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Event>,
}
impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}
impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }
    pub fn publish(&self, event: &str, data: serde_json::Value) {
        let _ = self.sender.send(Event {
            v: 1,
            event: event.into(),
            at: now(),
            data,
        });
    }
}
pub(crate) fn now() -> String {
    Local::now().to_rfc3339()
}
