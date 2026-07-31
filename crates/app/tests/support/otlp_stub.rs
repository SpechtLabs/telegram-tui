//! An in-process OTLP/HTTP collector, for `telemetry_allowlist.rs` (plan T52,
//! spec §13.8). It is what stands between "the exporter believes it sent the
//! right thing" and "the right thing arrived on the wire".
//!
//! # Why it decodes rather than greps
//!
//! `otel.rs`'s own tests assert against the raw request bytes, which is
//! enough to say "this string is in there" but cannot say "and nothing else
//! is". The allowlist claim is a claim about the *complement* — no key
//! outside `ALLOWED_KEYS`, anywhere — and that can only be checked by
//! decoding the payload and enumerating what it holds. So this stub parses
//! `ExportLogsServiceRequest` with `opentelemetry-proto` and records every
//! attribute at all three levels (resource, instrumentation scope, log
//! record), plus each record's body, severity and event name.
//!
//! The raw bodies are kept as well: a substring search over the exact bytes
//! that left the process is the one check that survives a decoding mistake
//! on this side, and `install_id_present_chat_ids_absent` uses it.
//!
//! # Why it runs on its own thread and its own runtime
//!
//! The OTLP exporter ships from a dedicated worker thread using a *blocking*
//! HTTP client, and `OtelGuard::shutdown` blocks the caller until that
//! worker has flushed. A collector spawned onto the test's own
//! `#[tokio::test]` runtime (single-threaded) would therefore never be
//! polled while the test waits for the flush: the request would sit unread,
//! shutdown would hit its two-second ceiling, and the assertions would run
//! against an empty collector. Giving the stub its own thread and its own
//! runtime removes the dependency entirely — it answers whether or not the
//! test thread is blocked.
//!
//! # `/v1/traces`
//!
//! The pipeline is logs-only (`emit!` produces `tracing` events, not spans),
//! so nothing should ever arrive there. The route exists so that a
//! regression which starts a trace pipeline is *observed* — landing in
//! [`OtlpStub::unexpected`] and failing the subset test — rather than
//! silently 404'd and lost.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::post;
use opentelemetry_proto::tonic::collector::logs::v1::{
    ExportLogsServiceRequest, ExportLogsServiceResponse,
};
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
use prost::Message;

/// How long [`OtlpStub::wait_for`] gives a condition before it gives up.
/// Every export the tests trigger is forced by an explicit flush, so this
/// only bounds a hang.
const WAIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Which level of the OTLP envelope an attribute was attached to. The
/// allowlist does not distinguish them — a leak is a leak wherever it rides
/// — but a failure message that says *where* the stray key came from is the
/// difference between a one-line fix and a hunt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Resource,
    Scope,
    Record,
    /// An OTLP/JSON payload, whose structure this stub walks generically.
    /// See [`collect_json_attributes`] for why the level is not recovered.
    Json,
}

#[derive(Debug, Clone)]
pub struct Attribute {
    pub level: Level,
    pub key: String,
    pub value: String,
}

/// One exported log record, flattened to what an allowlist review cares
/// about.
#[derive(Debug, Clone)]
pub struct Record {
    /// The instrumentation scope, which the tracing bridge sets from the
    /// event's `target` — `"tgt_telemetry"` for everything `emit!` produces.
    pub scope: String,
    /// OTLP's `event_name`, which the bridge sets from `tracing`'s callsite
    /// name.
    pub event_name: String,
    pub severity: String,
    /// `emit!` carries no `message` field, so this is expected to be `None`
    /// on every record — a `Some` means someone gave a telemetry event a
    /// formatted message, which is free-form text by definition.
    pub body: Option<String>,
    pub attributes: Vec<(String, String)>,
}

impl Record {
    /// The record's `action` attribute, which every `emit!` event carries.
    pub fn action(&self) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == "action")
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Debug, Default)]
struct Collected {
    requests: usize,
    attributes: Vec<Attribute>,
    records: Vec<Record>,
    bodies: Vec<Vec<u8>>,
    /// Anything that arrived somewhere other than `/v1/logs`, and anything
    /// whose content type this stub could not decode. Both are assertion
    /// material, not stub bugs to be swallowed.
    unexpected: Vec<String>,
}

/// A running collector. Dropping it shuts the server down and joins its
/// thread.
pub struct OtlpStub {
    endpoint: String,
    collected: Arc<Mutex<Collected>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl OtlpStub {
    /// Binds an ephemeral port and starts serving. The listener is bound on
    /// the caller's thread so [`endpoint`](Self::endpoint) is known — and
    /// the port is already accepting connections — before this returns.
    pub fn start() -> OtlpStub {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        listener
            .set_nonblocking(true)
            .expect("a nonblocking listener for tokio to adopt");
        let endpoint = format!("http://{}", listener.local_addr().expect("a bound address"));

        let collected = Arc::new(Mutex::new(Collected::default()));
        let state = Arc::clone(&collected);
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();

        let thread = std::thread::Builder::new()
            .name("otlp-stub".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("a runtime for the stub");
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)
                        .expect("adopt the bound listener");
                    let app = Router::new()
                        .route("/v1/logs", post(logs))
                        .route("/v1/traces", post(traces))
                        .fallback(elsewhere)
                        .with_state(state);
                    let _ = axum::serve(listener, app)
                        .with_graceful_shutdown(async move {
                            let _ = shutdown_rx.await;
                        })
                        .await;
                });
            })
            .expect("spawn the stub thread");

        OtlpStub {
            endpoint,
            collected,
            shutdown: Some(shutdown),
            thread: Some(thread),
        }
    }

    /// The base URL to configure as a custom telemetry destination.
    /// `otel::logs_endpoint` appends `/v1/logs` to it.
    pub fn endpoint(&self) -> String {
        self.endpoint.clone()
    }

    pub fn request_count(&self) -> usize {
        self.collected.lock().unwrap().requests
    }

    /// Every attribute key seen anywhere in every request — the set the
    /// allowlist assertion is about.
    pub fn keys(&self) -> BTreeSet<String> {
        self.collected
            .lock()
            .unwrap()
            .attributes
            .iter()
            .map(|attribute| attribute.key.clone())
            .collect()
    }

    pub fn attributes(&self) -> Vec<Attribute> {
        self.collected.lock().unwrap().attributes.clone()
    }

    pub fn records(&self) -> Vec<Record> {
        self.collected.lock().unwrap().records.clone()
    }

    /// The `action` of every exported record, in arrival order.
    pub fn actions(&self) -> Vec<String> {
        self.records()
            .iter()
            .map(|record| record.action().unwrap_or("(no action)").to_string())
            .collect()
    }

    /// The exact bytes of every request body, for assertions that must not
    /// depend on this stub having decoded anything correctly.
    pub fn bodies(&self) -> Vec<Vec<u8>> {
        self.collected.lock().unwrap().bodies.clone()
    }

    pub fn unexpected(&self) -> Vec<String> {
        self.collected.lock().unwrap().unexpected.clone()
    }

    /// The value of `key` wherever it appears, deduplicated.
    pub fn values_of(&self, key: &str) -> BTreeSet<String> {
        self.collected
            .lock()
            .unwrap()
            .attributes
            .iter()
            .filter(|attribute| attribute.key == key)
            .map(|attribute| attribute.value.clone())
            .collect()
    }

    /// Blocks until `done` holds, or fails the test. Used to reach
    /// quiescence without a fixed sleep: the exporter's flush is what
    /// actually delivers, and this only covers the gap between the flush
    /// returning and the handler finishing its bookkeeping.
    pub fn wait_for(&self, what: &str, done: impl Fn(&OtlpStub) -> bool) {
        let deadline = Instant::now() + WAIT_TIMEOUT;
        while !done(self) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}\n  requests: {}\n  keys: {:?}\n  unexpected: {:?}",
                self.request_count(),
                self.keys(),
                self.unexpected(),
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for OtlpStub {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

type SharedState = Arc<Mutex<Collected>>;

async fn logs(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    ingest(&state, &headers, &body);
    // A protobuf-encoded empty response, which is what an OTLP/HTTP client
    // expects back from a fully accepted export. `encode_to_vec` on the
    // default value produces zero bytes, which is the correct encoding of a
    // message with no partial success.
    (
        [(axum::http::header::CONTENT_TYPE, "application/x-protobuf")],
        ExportLogsServiceResponse::default().encode_to_vec(),
    )
}

async fn traces(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    state
        .lock()
        .unwrap()
        .unexpected
        .push("a trace export reached /v1/traces".to_string());
    ingest(&state, &headers, &body);
    (
        [(axum::http::header::CONTENT_TYPE, "application/x-protobuf")],
        Vec::new(),
    )
}

async fn elsewhere(State(state): State<SharedState>, request: axum::extract::Request) -> String {
    state
        .lock()
        .unwrap()
        .unexpected
        .push(format!("{} {}", request.method(), request.uri().path()));
    String::new()
}

fn ingest(state: &SharedState, headers: &HeaderMap, body: &[u8]) {
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();

    let mut collected = state.lock().unwrap();
    collected.requests += 1;
    collected.bodies.push(body.to_vec());

    if content_type.contains("json") {
        collect_json(&mut collected, body);
    } else if content_type.contains("protobuf") || content_type.is_empty() {
        collect_protobuf(&mut collected, body);
    } else {
        collected
            .unexpected
            .push(format!("undecodable content type {content_type:?}"));
    }
}

fn collect_protobuf(collected: &mut Collected, body: &[u8]) {
    let request = match ExportLogsServiceRequest::decode(body) {
        Ok(request) => request,
        Err(err) => {
            collected
                .unexpected
                .push(format!("body did not decode as OTLP protobuf: {err}"));
            return;
        }
    };

    for resource_logs in request.resource_logs {
        if let Some(resource) = resource_logs.resource {
            push_attributes(collected, Level::Resource, &resource.attributes);
        }
        for scope_logs in resource_logs.scope_logs {
            let scope = scope_logs.scope.unwrap_or_default();
            push_attributes(collected, Level::Scope, &scope.attributes);
            for log_record in scope_logs.log_records {
                push_attributes(collected, Level::Record, &log_record.attributes);
                collected.records.push(Record {
                    scope: scope.name.clone(),
                    event_name: log_record.event_name,
                    severity: log_record.severity_text,
                    body: log_record.body.as_ref().map(render),
                    attributes: log_record
                        .attributes
                        .iter()
                        .map(|kv| {
                            (
                                kv.key.clone(),
                                kv.value.as_ref().map(render).unwrap_or_default(),
                            )
                        })
                        .collect(),
                });
            }
        }
    }
}

fn push_attributes(collected: &mut Collected, level: Level, attributes: &[KeyValue]) {
    for kv in attributes {
        collected.attributes.push(Attribute {
            level,
            key: kv.key.clone(),
            value: kv.value.as_ref().map(render).unwrap_or_default(),
        });
    }
}

/// An `AnyValue` as a plain string. Nested values are rendered structurally
/// rather than skipped: an allowlisted key whose value is a map of
/// un-allowlisted keys would otherwise walk straight past every assertion.
fn render(value: &AnyValue) -> String {
    match &value.value {
        Some(any_value::Value::StringValue(text)) => text.clone(),
        Some(any_value::Value::BoolValue(flag)) => flag.to_string(),
        Some(any_value::Value::IntValue(number)) => number.to_string(),
        Some(any_value::Value::DoubleValue(number)) => number.to_string(),
        Some(any_value::Value::BytesValue(bytes)) => format!("{bytes:?}"),
        Some(any_value::Value::ArrayValue(array)) => {
            let items: Vec<String> = array.values.iter().map(render).collect();
            format!("[{}]", items.join(", "))
        }
        Some(any_value::Value::KvlistValue(list)) => {
            let items: Vec<String> = list
                .values
                .iter()
                .map(|kv| {
                    format!(
                        "{}={}",
                        kv.key,
                        kv.value.as_ref().map(render).unwrap_or_default()
                    )
                })
                .collect();
            format!("{{{}}}", items.join(", "))
        }
        Some(other) => format!("{other:?}"),
        None => String::new(),
    }
}

/// OTLP/JSON, which `protocol = "http/json"` selects (spec §13.5).
///
/// This walks the document generically rather than deserializing it into the
/// generated types, because doing the latter needs `opentelemetry-proto`'s
/// `with-serde` feature — which this crate gets only incidentally, as a
/// side effect of `opentelemetry-otlp`'s `http-json`. Depending on a
/// transitive feature to decode the payload would mean the proof quietly
/// stops covering the JSON transport the day that dependency is reorganised.
fn collect_json(collected: &mut Collected, body: &[u8]) {
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(body) else {
        collected
            .unexpected
            .push("body did not parse as JSON".to_string());
        return;
    };
    collect_json_attributes(collected, &document);
}

/// Every `{"key": ..., "value": ...}` object in the document, at any depth.
/// That shape is exactly OTLP/JSON's `KeyValue`, and it is the same shape at
/// all three levels — which is why the [`Level`] is not recovered here. The
/// subset assertion is over the union of keys, so it does not need one.
fn collect_json_attributes(collected: &mut Collected, value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            if let (Some(serde_json::Value::String(key)), Some(json_value)) =
                (fields.get("key"), fields.get("value"))
            {
                collected.attributes.push(Attribute {
                    level: Level::Json,
                    key: key.clone(),
                    value: render_json(json_value),
                });
            }
            for nested in fields.values() {
                collect_json_attributes(collected, nested);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_json_attributes(collected, item);
            }
        }
        _ => {}
    }
}

/// An OTLP/JSON `AnyValue` (`{"stringValue": "x"}`, `{"intValue": "3"}`, …)
/// as a plain string. The wrapper is unwrapped when there is exactly one, so
/// values compare the same way they do on the protobuf path.
fn render_json(value: &serde_json::Value) -> String {
    if let serde_json::Value::Object(fields) = value
        && fields.len() == 1
        && let Some(inner) = fields.values().next()
    {
        return match inner {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        };
    }
    value.to_string()
}
