//! `FakeTd`: JSONL fixture replay implementing `TdRuntime`. See
//! docs/architecture.md §4.7 and §7.
//!
//! # Fixture format
//!
//! One serde-JSON [`ScriptStep`] per line; blank lines are skipped. `Emit`
//! steps push an update to the updates channel; `Await` steps block the
//! script cursor until a request matching `expect` arrives, then answer it
//! with `respond`.
//!
//! # Driver mechanics
//!
//! The script is driven synchronously: leading `Emit` steps are pushed to
//! the updates channel immediately at construction time (not lazily on the
//! first `updates()` call), stopping at the first `Await` or the end of the
//! script. Each time a request matches the current `Await`, the cursor
//! advances past it and the driver resumes pushing subsequent `Emit` steps
//! until the next `Await` or the end.
//!
//! Because pushes happen outside of an `async` context (construction is a
//! plain function, and `request()` never needs to block on send), the driver
//! uses `mpsc::Sender::try_send` rather than an awaited `send`. The channel
//! capacity ([`CHANNEL_CAPACITY`]) is generous (1024) so ordinary fixtures
//! never fill it; if a fixture ever pushes more than that many consecutive
//! `Emit` steps without an intervening `Await`, the driver panics rather
//! than silently dropping an update — that is always a fixture bug, not a
//! runtime condition tests should tolerate.
//!
//! # Locking
//!
//! Interior state lives behind a plain `std::sync::Mutex`, not a `tokio`
//! mutex: `TdRuntime::updates()` is a synchronous trait method, and a
//! `tokio::sync::Mutex` would require `blocking_lock()` there, which panics
//! if called from inside a Tokio runtime — exactly where this runs. A `std`
//! mutex is safe because no guard is ever held across an `.await` point;
//! every push to the updates channel is the non-blocking `try_send` above.

use crate::td::error::TdError;
use crate::td::request::{TdRequest, TdResponse};
use crate::td::runtime::TdRuntime;
use crate::td::update::TdUpdate;
use async_trait::async_trait;
use serde::de::Error as _;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tokio::sync::mpsc;

/// Updates-channel capacity. See module docs, "Driver mechanics".
const CHANNEL_CAPACITY: usize = 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ScriptStep {
    /// Push this update to the updates channel immediately.
    Emit(TdUpdate),
    /// Block until a request matching `expect` arrives, then answer it.
    Await {
        expect: RequestMatcher,
        respond: RespondWith,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RequestMatcher {
    Any,
    /// Discriminant-only match ("a GetChatHistory, whatever its params").
    Kind(String),
    Exact(TdRequest),
}

// Boxing `Ok(TdResponse)` would deviate from the verbatim contract in
// docs/architecture.md §4.7, matching the same allow on `TdResponse` itself
// in td/request.rs; the size skew is accepted instead.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RespondWith {
    Ok(TdResponse),
    Err(TdError),
}

/// Mutable fixture-cursor state, guarded by a plain mutex. See module docs,
/// "Locking".
#[derive(Debug)]
struct State {
    steps: Vec<ScriptStep>,
    cursor: usize,
    received: Vec<TdRequest>,
}

#[derive(Debug)]
pub struct FakeTd {
    tx: mpsc::Sender<TdUpdate>,
    rx: Mutex<Option<mpsc::Receiver<TdUpdate>>>,
    state: Mutex<State>,
}

impl FakeTd {
    pub fn from_jsonl(fixture: &str) -> Result<Self, serde_json::Error> {
        let mut steps = Vec::new();
        for (idx, line) in fixture.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let step: ScriptStep = serde_json::from_str(trimmed)
                .map_err(|e| serde_json::Error::custom(format!("line {}: {}", idx + 1, e)))?;
            steps.push(step);
        }

        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let mut state = State {
            steps,
            cursor: 0,
            received: Vec::new(),
        };
        drain_emits(&tx, &mut state);

        Ok(FakeTd {
            tx,
            rx: Mutex::new(Some(rx)),
            state: Mutex::new(state),
        })
    }

    /// Every request ever received, for post-hoc assertions.
    pub fn received(&self) -> Vec<TdRequest> {
        self.state.lock().unwrap().received.clone()
    }
}

/// Push leading `Emit` steps from the cursor onward, stopping at the first
/// `Await` or the end of the script.
fn drain_emits(tx: &mpsc::Sender<TdUpdate>, state: &mut State) {
    while let Some(step) = state.steps.get(state.cursor) {
        match step {
            ScriptStep::Emit(update) => {
                let update = update.clone();
                if tx.try_send(update).is_err() {
                    panic!(
                        "FakeTd: updates channel full or closed while driving script; \
                         increase CHANNEL_CAPACITY or add an Await step to the fixture"
                    );
                }
                state.cursor += 1;
            }
            ScriptStep::Await { .. } => break,
        }
    }
}

fn matches_request(matcher: &RequestMatcher, req: &TdRequest) -> bool {
    match matcher {
        RequestMatcher::Any => true,
        RequestMatcher::Kind(kind) => req.kind() == kind,
        RequestMatcher::Exact(expected) => expected == req,
    }
}

#[async_trait]
impl TdRuntime for FakeTd {
    async fn request(&self, req: TdRequest) -> Result<TdResponse, TdError> {
        let mut state = self.state.lock().unwrap();
        state.received.push(req.clone());

        let is_match = matches!(
            state.steps.get(state.cursor),
            Some(ScriptStep::Await { expect, .. }) if matches_request(expect, &req)
        );
        if !is_match {
            return Ok(TdResponse::Ok);
        }

        let respond = match state.steps[state.cursor].clone() {
            ScriptStep::Await { respond, .. } => respond,
            ScriptStep::Emit(_) => unreachable!("cursor always rests on Await or end"),
        };
        state.cursor += 1;
        drain_emits(&self.tx, &mut state);

        match respond {
            RespondWith::Ok(resp) => Ok(resp),
            RespondWith::Err(err) => Err(err),
        }
    }

    /// Called exactly once by the runtime loop at boot; panics on second
    /// call.
    fn updates(&self) -> mpsc::Receiver<TdUpdate> {
        self.rx
            .lock()
            .unwrap()
            .take()
            .expect("FakeTd::updates() called twice")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::chat::ChatListId;
    use crate::model::ids::ChatId;
    use crate::td::update::ConnectionPhase;

    fn fixture(steps: &[ScriptStep]) -> String {
        steps
            .iter()
            .map(|s| serde_json::to_string(s).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn emit_steps_arrive_in_order() {
        let steps = vec![
            ScriptStep::Emit(TdUpdate::Connection(ConnectionPhase::Connecting)),
            ScriptStep::Emit(TdUpdate::Connection(ConnectionPhase::Ready)),
        ];
        let fake = FakeTd::from_jsonl(&fixture(&steps)).unwrap();
        let mut rx = fake.updates();

        assert_eq!(
            rx.recv().await.unwrap(),
            TdUpdate::Connection(ConnectionPhase::Connecting)
        );
        assert_eq!(
            rx.recv().await.unwrap(),
            TdUpdate::Connection(ConnectionPhase::Ready)
        );
    }

    #[tokio::test]
    async fn await_matches_kind_and_responds() {
        let steps = vec![
            ScriptStep::Emit(TdUpdate::Connection(ConnectionPhase::Connecting)),
            ScriptStep::Await {
                expect: RequestMatcher::Kind("LoadChats".to_string()),
                respond: RespondWith::Ok(TdResponse::Chats {
                    chat_ids: vec![ChatId(1)],
                }),
            },
            ScriptStep::Emit(TdUpdate::Connection(ConnectionPhase::Ready)),
        ];
        let fake = FakeTd::from_jsonl(&fixture(&steps)).unwrap();
        let mut rx = fake.updates();

        assert_eq!(
            rx.recv().await.unwrap(),
            TdUpdate::Connection(ConnectionPhase::Connecting)
        );
        // The trailing Emit must not have arrived yet.
        assert!(rx.try_recv().is_err());

        let resp = fake
            .request(TdRequest::LoadChats {
                list: ChatListId::Main,
                limit: 50,
            })
            .await
            .unwrap();
        assert_eq!(
            resp,
            TdResponse::Chats {
                chat_ids: vec![ChatId(1)]
            }
        );

        assert_eq!(
            rx.recv().await.unwrap(),
            TdUpdate::Connection(ConnectionPhase::Ready)
        );
    }

    #[tokio::test]
    async fn await_exact_mismatch_gets_default_ok_and_is_recorded() {
        let target = TdRequest::OpenChat { chat_id: ChatId(1) };
        let steps = vec![ScriptStep::Await {
            expect: RequestMatcher::Exact(target.clone()),
            respond: RespondWith::Ok(TdResponse::Chats {
                chat_ids: vec![ChatId(99)],
            }),
        }];
        let fake = FakeTd::from_jsonl(&fixture(&steps)).unwrap();

        let mismatched = TdRequest::OpenChat { chat_id: ChatId(2) };
        let resp = fake.request(mismatched.clone()).await.unwrap();
        assert_eq!(resp, TdResponse::Ok);
        assert_eq!(fake.received(), vec![mismatched]);

        // Cursor is unmoved: the scripted response still fires for the
        // matching request.
        let resp = fake.request(target.clone()).await.unwrap();
        assert_eq!(
            resp,
            TdResponse::Chats {
                chat_ids: vec![ChatId(99)]
            }
        );
        assert_eq!(
            fake.received(),
            vec![TdRequest::OpenChat { chat_id: ChatId(2) }, target]
        );
    }

    #[tokio::test]
    async fn malformed_jsonl_line_reports_line_number() {
        let valid = serde_json::to_string(&ScriptStep::Emit(TdUpdate::Connection(
            ConnectionPhase::Connecting,
        )))
        .unwrap();
        let fixture = format!("{valid}\n\nnot valid json");

        let err = FakeTd::from_jsonl(&fixture).unwrap_err();
        assert!(
            err.to_string().contains('3'),
            "expected line number 3 in error, got: {err}"
        );
    }
}
