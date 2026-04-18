//! Event hub — tokio broadcast channel + subscriber management.
//!
//! Every state change and error in the daemon is published through the hub.
//! `StreamService::SubscribeEvents` subscribes to the hub and forwards
//! matching events to the gRPC stream.

use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast;
use tracing::warn;

// ── Event types ───────────────────────────────────────────────────────────────

/// The kind of an internal daemon event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    LabState,
    NodeState,
    #[allow(dead_code)] // used by Phase 10 Chaos DSL
    Chaos,
    Error,
}

impl EventKind {
    #[allow(dead_code)] // retained for diagnostic / logging use
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::LabState => "LAB_STATE",
            EventKind::NodeState => "NODE_STATE",
            EventKind::Chaos => "CHAOS",
            EventKind::Error => "ERROR",
        }
    }

    /// Convert to the proto `EventKind` i32 value.
    pub fn to_proto_i32(self) -> i32 {
        use themis_proto::EventKind as P;
        match self {
            EventKind::LabState => P::LabState as i32,
            EventKind::NodeState => P::NodeState as i32,
            EventKind::Chaos => P::Chaos as i32,
            EventKind::Error => P::Error as i32,
        }
    }
}

/// A single daemon event.
#[derive(Debug, Clone)]
pub struct Event {
    pub timestamp_ns: i64,
    pub lab: String,
    pub kind: EventKind,
    pub subject: String,
    pub message: String,
    pub payload: Vec<u8>,
}

impl Event {
    /// Convert to the proto wire type.
    pub fn to_proto(&self) -> themis_proto::Event {
        themis_proto::Event {
            timestamp_unix_ns: self.timestamp_ns,
            lab: self.lab.clone(),
            kind: self.kind.to_proto_i32(),
            subject: self.subject.clone(),
            message: self.message.clone(),
            payload: self.payload.clone(),
        }
    }
}

// ── EventHub ──────────────────────────────────────────────────────────────────

/// Broadcast hub for daemon events.
///
/// Publishers call `publish`; subscribers call `subscribe` to get a
/// `broadcast::Receiver<Event>`.
#[derive(Clone)]
pub struct EventHub {
    tx: broadcast::Sender<Event>,
}

impl EventHub {
    /// Create a new hub with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    /// Publish an event to all current subscribers.
    ///
    /// If no subscribers are present the send is silently dropped.
    pub async fn publish(
        &self,
        lab: &str,
        kind: EventKind,
        subject: &str,
        message: &str,
        payload: Vec<u8>,
    ) {
        let event = Event {
            timestamp_ns: now_ns(),
            lab: lab.to_string(),
            kind,
            subject: subject.to_string(),
            message: message.to_string(),
            payload,
        };
        if let Err(_e) = self.tx.send(event) {
            // No subscribers — this is fine.
        }
    }

    /// Subscribe to the event stream.
    ///
    /// Returns a `broadcast::Receiver<Event>`. The receiver will miss events
    /// sent before `subscribe()` is called, but there is no replay
    /// mechanism (events are ephemeral in the channel; the `events` SQLite
    /// table holds the permanent log).
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    /// Subscribe and forward matching events as proto `Event` messages into a
    /// `tokio::sync::mpsc` channel that backs a gRPC streaming response.
    ///
    /// Filtering:
    ///   - `lab_filter`: if non-empty, only events where `event.lab == lab_filter`.
    ///   - `kind_filter`: if non-empty, only events whose `kind` is in the set.
    ///
    /// Runs until the mpsc sender is dropped (client disconnected) or the
    /// broadcast channel is closed (daemon shutting down).
    pub async fn stream_events(
        &self,
        lab_filter: String,
        kind_filter: Vec<i32>,
        out: tokio::sync::mpsc::Sender<Result<themis_proto::Event, tonic::Status>>,
    ) {
        let mut rx = self.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    // Apply filters.
                    if !lab_filter.is_empty() && event.lab != lab_filter {
                        continue;
                    }
                    if !kind_filter.is_empty() && !kind_filter.contains(&event.kind.to_proto_i32()) {
                        continue;
                    }
                    if out.send(Ok(event.to_proto())).await.is_err() {
                        // Client disconnected.
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!("event subscriber lagged by {n} messages");
                    // Continue — we've dropped some events but the stream
                    // is still alive. Report a gap event if desired in future.
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // Hub closed (daemon shutting down).
                    break;
                }
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as i64
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_and_receive() {
        let hub = EventHub::new(16);
        let mut rx = hub.subscribe();

        hub.publish("my-lab", EventKind::LabState, "my-lab", "test message", vec![])
            .await;

        let event = rx.try_recv().expect("event should be immediately available");
        assert_eq!(event.lab, "my-lab");
        assert_eq!(event.kind, EventKind::LabState);
        assert_eq!(event.subject, "my-lab");
        assert_eq!(event.message, "test message");
    }

    #[tokio::test]
    async fn no_subscribers_does_not_panic() {
        let hub = EventHub::new(16);
        // No subscribers registered — publish should succeed silently.
        hub.publish("lab", EventKind::Error, "lab", "boom", vec![])
            .await;
    }

    #[tokio::test]
    async fn multiple_subscribers_each_receive() {
        let hub = EventHub::new(16);
        let mut rx1 = hub.subscribe();
        let mut rx2 = hub.subscribe();

        hub.publish("lab", EventKind::NodeState, "node-1", "up", vec![]).await;

        let e1 = rx1.try_recv().expect("rx1 should receive");
        let e2 = rx2.try_recv().expect("rx2 should receive");
        assert_eq!(e1.subject, e2.subject);
    }

    #[tokio::test]
    async fn stream_events_filters_by_lab() {
        let hub = EventHub::new(32);
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);

        // Subscribe first so we don't miss events that are published before
        // the streaming task's internal subscribe call runs.
        let broadcast_rx = hub.subscribe();

        let hub2 = hub.clone();
        let filter_tx = tx.clone();
        tokio::spawn(async move {
            hub2.stream_events("lab-a".to_string(), vec![], filter_tx).await;
        });

        // Yield to let the spawned task call subscribe() inside stream_events.
        tokio::task::yield_now().await;
        drop(broadcast_rx); // release our sentinel receiver

        // Publish one for lab-a and one for lab-b.
        hub.publish("lab-a", EventKind::LabState, "lab-a", "A event", vec![]).await;
        hub.publish("lab-b", EventKind::LabState, "lab-b", "B event", vec![]).await;

        // Wait for events to flow through.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Only lab-a event should arrive.
        let proto_event = rx.try_recv().expect("lab-a event expected");
        assert_eq!(proto_event.unwrap().lab, "lab-a");

        // lab-b event must be filtered.
        assert!(rx.try_recv().is_err(), "lab-b event should be filtered");
    }

    #[tokio::test]
    async fn stream_events_filters_by_kind() {
        let hub = EventHub::new(32);
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);

        let broadcast_rx = hub.subscribe();

        let hub2 = hub.clone();
        let filter_tx = tx.clone();
        tokio::spawn(async move {
            hub2.stream_events(
                String::new(),
                vec![themis_proto::EventKind::LabState as i32],
                filter_tx,
            )
            .await;
        });

        tokio::task::yield_now().await;
        drop(broadcast_rx);

        hub.publish("lab", EventKind::LabState, "lab", "state event", vec![]).await;
        hub.publish("lab", EventKind::Error, "lab", "error event", vec![]).await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let event = rx.try_recv().expect("lab-state event expected");
        assert_eq!(
            event.unwrap().kind,
            themis_proto::EventKind::LabState as i32
        );
        assert!(rx.try_recv().is_err(), "error event should be filtered");
    }

    #[test]
    fn event_kind_round_trip() {
        let kinds = [
            EventKind::LabState,
            EventKind::NodeState,
            EventKind::Chaos,
            EventKind::Error,
        ];
        for k in kinds {
            let proto_val = k.to_proto_i32();
            assert!(proto_val > 0, "proto value should be non-zero for {k:?}");
        }
    }

    #[test]
    fn to_proto_maps_fields() {
        let event = Event {
            timestamp_ns: 42,
            lab: "lab-x".into(),
            kind: EventKind::Chaos,
            subject: "link-1".into(),
            message: "flap started".into(),
            payload: vec![1, 2, 3],
        };
        let proto = event.to_proto();
        assert_eq!(proto.timestamp_unix_ns, 42);
        assert_eq!(proto.lab, "lab-x");
        assert_eq!(proto.subject, "link-1");
        assert_eq!(proto.message, "flap started");
        assert_eq!(proto.payload, vec![1u8, 2, 3]);
    }
}
