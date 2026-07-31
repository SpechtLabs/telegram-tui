//! The OTLP exporter: the only thing in this binary that puts bytes on the
//! network on the app's behalf. See docs/architecture.md §2.3, §4.8; spec
//! §13.1, §13.5-§13.7.
//!
//! # Why this is not `tracing-batteries`
//!
//! Spec §13.1 sketches a `tracing_batteries::Session` with the
//! `OpenTelemetry` battery. That crate is still a dependency (§6.4 pins it),
//! but its battery cannot be used here, for two reasons visible in its
//! source at rev `f059e936`:
//!
//! 1. `OpenTelemetry::setup` builds its *own* `tracing_subscriber::registry()`
//!    and calls `.init()` on it. Only one subscriber can be global, so the
//!    battery and `logging::init`'s rolling file layer are mutually
//!    exclusive — whichever runs second panics. The battery exposes no way
//!    to hand its layers back for composition.
//! 2. The battery's layers are filtered only by level (`LOG_LEVEL`), so
//!    every `tracing::info!` in the process would be shipped to the
//!    collector. That is exactly the leak spec §13.2 exists to prevent: a
//!    stray `info!("opening chat {}", title)` would reach the network.
//!
//! So this module drives the same underlying stack (`opentelemetry_sdk` +
//! `opentelemetry-otlp` + `opentelemetry-appender-tracing`, the crates the
//! battery itself wraps) directly, which lets the export layer be filtered
//! and composed into the one registry `logging::init` installs.
//!
//! # Shape of the pipeline
//!
//! `emit!` events are `tracing` *events*, not spans, so they travel as OTLP
//! **log records** (`OpenTelemetryTracingBridge`), one record per event with
//! the event's fields as attributes. Session-constant, allowlisted values
//! (`install.id`, `session.id`, `app.version`, `term.*`) ride on the
//! resource instead of being repeated on every record.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use color_eyre::eyre::{self, Context};
use etcetera::BaseStrategy;
use opentelemetry::KeyValue;
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, Protocol, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::logs::{BatchConfigBuilder, BatchLogProcessor, SdkLoggerProvider};
use tgt_core::effect::TelemetryMode;
use tgt_core::telemetry::schema::keys;
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context as LayerContext;
use tracing_subscriber::registry::LookupSpan;

use crate::logging::ExportLayer;

/// The target `tgt_core::emit!` stamps on every telemetry event. It is a
/// string literal inside that macro, so this constant restates it; the
/// filter test below is what keeps the two honest.
pub const TELEMETRY_TARGET: &str = "tgt_telemetry";

/// Vendor ingest proxy (spec §13.6), baked in at build time. A build
/// without it produces a binary whose vendor mode is inert.
const VENDOR_ENDPOINT: Option<&str> = option_env!("TGT_INGEST_ENDPOINT");

/// Sent with every request so the ingest proxy can reject or rate-limit by
/// client version without a release (spec §13.1, §13.6).
const CLIENT_HEADER: &str = "x-tgt-client";

/// Hard ceiling on shutdown (spec §13.7). Quitting must not wait on a
/// retrying exporter.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded queue between the emitting thread and the export thread. Full
/// means drop, never block (spec §13.7). A session emits on the order of
/// one event per user action, so 512 is generous.
const MAX_QUEUE_SIZE: usize = 512;

/// How long a batch waits to fill before being sent.
const SCHEDULED_DELAY: Duration = Duration::from_secs(5);

const APP_DIR: &str = "telegram-tui";
const INSTALL_ID_FILE: &str = "install-id";
const SALT_FILE: &str = "telemetry-salt";

/// The session-constant, allowlisted values that ride on the OTLP resource.
/// Everything here is drawn from `schema::keys`; nothing else is ever
/// attached at session scope.
pub struct SessionContext {
    pub install_id: String,
    pub session_id: String,
    /// `$TERM_PROGRAM`, when the terminal sets it.
    pub term_program: Option<String>,
    /// `graphics::telemetry_str` output: kitty|iterm2|sixel|none.
    pub graphics_protocol: &'static str,
    /// `schema::buckets::width` output.
    pub width_bucket: &'static str,
}

/// A user-configured destination which, under `mode = "custom"`, *replaces*
/// the vendor one. Data is never dual-shipped (spec §13.5).
pub struct CustomEndpoint {
    pub endpoint: String,
    /// `http/protobuf` or `http/json`; `None` means http/protobuf.
    pub protocol: Option<String>,
    pub headers: Vec<(String, String)>,
}

/// A live exporter: the layer to compose into the subscriber, and the guard
/// whose shutdown flushes it.
pub struct Exporter {
    pub layer: ExportLayer,
    pub guard: OtelGuard,
}

/// Per-install identity (spec §13.4, §13.5): a pseudonymous id that is
/// exported, and an HMAC salt that never is.
pub struct Identity {
    pub install_id: String,
    pub salt: [u8; 32],
}

/// True for events the exporter is allowed to see: the `emit!` target *and*
/// the public marker field. Both are required — a raw `tracing` macro can
/// reach neither without deliberately impersonating `emit!`.
///
/// Every input is callsite-static, so this is a pure function of metadata
/// and returns the same answer for the life of a callsite.
pub fn is_public_telemetry(metadata: &Metadata<'_>) -> bool {
    metadata.target() == TELEMETRY_TARGET && metadata.fields().field(keys::PUBLIC_MARKER).is_some()
}

/// Wraps a layer so it only ever sees events that pass
/// [`is_public_telemetry`]. Span callbacks are deliberately not forwarded:
/// only events carry the marker, and a span cannot be exported.
///
/// The check lives in `on_event` rather than in `register_callsite` or
/// `enabled` on purpose. Those two are consulted by `Layered` for the whole
/// subscriber, so a `never` answer here would also silence the rolling file
/// log — the sink that is supposed to see everything.
pub struct PublicOnly<L> {
    inner: L,
}

impl<L> PublicOnly<L> {
    pub fn new(inner: L) -> Self {
        Self { inner }
    }
}

impl<L, S> Layer<S> for PublicOnly<L>
where
    L: Layer<S>,
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: LayerContext<'_, S>) {
        if is_public_telemetry(event.metadata()) {
            self.inner.on_event(event, ctx);
        }
    }
}

/// Flushes and stops the export pipeline within [`SHUTDOWN_TIMEOUT`].
///
/// Prefer the explicit [`OtelGuard::shutdown`] from `main`, which runs while
/// the process is still orderly. `Drop` repeats it for the paths that miss
/// that call (an early `?`, a panic unwinding out of `run_tui`) — `Drop`
/// cannot await, so both routes go through the same blocking helper, which
/// bounds itself with a channel deadline rather than by joining the worker.
pub struct OtelGuard {
    provider: Option<SdkLoggerProvider>,
}

impl OtelGuard {
    /// Flushes and shuts the exporter down, blocking for at most
    /// [`SHUTDOWN_TIMEOUT`].
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        let Some(provider) = self.provider.take() else {
            return;
        };

        // `shutdown_with_timeout` already bounds itself, but it bounds the
        // *worker's* response, not this thread's wait in every failure mode
        // (a poisoned lock, a worker mid-`join`). Running it on a scratch
        // thread and waiting on a channel makes the ceiling ours: if the
        // deadline passes we walk away and let the process exit take the
        // thread with it.
        let (tx, rx) = mpsc::sync_channel(1);
        let spawned = std::thread::Builder::new()
            .name("otel-shutdown".to_string())
            .spawn(move || {
                let _ = tx.send(provider.shutdown_with_timeout(SHUTDOWN_TIMEOUT));
            });

        match spawned {
            Ok(_) => match rx.recv_timeout(SHUTDOWN_TIMEOUT) {
                Ok(Ok(())) => tracing::debug!("telemetry exporter flushed and shut down"),
                // Spec §13.7: export failures are a local debug line and
                // nothing else. The user is quitting; they do not care.
                Ok(Err(err)) => {
                    tracing::debug!(%err, "telemetry exporter shutdown reported an error")
                }
                Err(_) => tracing::debug!(
                    timeout_ms = SHUTDOWN_TIMEOUT.as_millis(),
                    "telemetry exporter shutdown timed out; abandoning it"
                ),
            },
            Err(err) => tracing::debug!(%err, "could not spawn the telemetry shutdown thread"),
        }
    }
}

impl Drop for OtelGuard {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

/// Builds the export pipeline for `mode`, or `Ok(None)` when this build and
/// configuration have nothing to export to (telemetry off, or a vendor build
/// without `TGT_INGEST_ENDPOINT`).
///
/// Callers must treat `Err` as "no telemetry", not as a startup failure: a
/// misconfigured collector is never a reason to refuse to run a chat client
/// (spec §13.7).
pub fn init(
    mode: TelemetryMode,
    session: &SessionContext,
    custom: Option<CustomEndpoint>,
) -> eyre::Result<Option<Exporter>> {
    let Some(destination) = destination(mode, custom) else {
        return Ok(None);
    };

    let provider = build_provider(&destination, session)?;
    let layer = PublicOnly::new(OpenTelemetryTracingBridge::new(&provider));

    tracing::debug!(
        mode = mode_str(mode),
        "telemetry exporter installed" // deliberately without the endpoint
    );

    Ok(Some(Exporter {
        layer: Box::new(layer),
        guard: OtelGuard {
            provider: Some(provider),
        },
    }))
}

/// Where this session exports to, if anywhere. Custom fully replaces vendor;
/// the two are never combined (spec §13.5).
fn destination(mode: TelemetryMode, custom: Option<CustomEndpoint>) -> Option<CustomEndpoint> {
    match mode {
        TelemetryMode::Off => None,
        TelemetryMode::Vendor => VENDOR_ENDPOINT.map(|endpoint| CustomEndpoint {
            endpoint: endpoint.to_string(),
            protocol: None,
            headers: Vec::new(),
        }),
        TelemetryMode::Custom => custom.filter(|c| !c.endpoint.trim().is_empty()),
    }
}

fn build_provider(
    destination: &CustomEndpoint,
    session: &SessionContext,
) -> eyre::Result<SdkLoggerProvider> {
    let mut builder = LogExporter::builder().with_http();

    // `opentelemetry-otlp` reads OTEL_EXPORTER_OTLP_{ENDPOINT,HEADERS,
    // PROTOCOL} itself, but programmatic configuration wins over the
    // environment there. Spec §13.5 promises the opposite for a user who
    // sets the standard variables, so the programmatic values are simply
    // withheld when the corresponding variable is present, and the SDK
    // resolves them. Nothing is applied twice.
    if !endpoint_set_in_env() {
        builder = builder.with_endpoint(logs_endpoint(&destination.endpoint));
    }
    if !protocol_set_in_env() {
        builder = builder.with_protocol(protocol_from(destination.protocol.as_deref())?);
    } else if env_protocol_is_grpc() {
        // gRPC is not compiled in for this transport; saying so beats
        // panicking somewhere inside the exporter at the first export.
        return Err(eyre::eyre!(
            "OTEL_EXPORTER_OTLP_PROTOCOL=grpc is not supported by this build; use http/protobuf or http/json"
        ));
    }

    let mut headers = vec![(
        CLIENT_HEADER.to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    )];
    headers.extend(destination.headers.iter().cloned());
    builder = builder.with_headers(headers.into_iter().collect());

    let exporter = builder
        .build()
        .map_err(|err| eyre::eyre!("failed to build the OTLP log exporter: {err}"))?;

    // Bounded queue, drop-on-full, exported from a dedicated worker thread
    // that `BatchLogProcessor` owns — no telemetry call ever blocks
    // `update()` or `view()` (spec §13.7). The bound is the SDK's:
    // `max_queue_size` sizes a `sync_channel`, and a full channel makes
    // `try_send` drop the record and bump a counter.
    let processor = BatchLogProcessor::builder(exporter)
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_max_queue_size(MAX_QUEUE_SIZE)
                .with_scheduled_delay(SCHEDULED_DELAY)
                .build(),
        )
        .build();

    Ok(SdkLoggerProvider::builder()
        .with_resource(resource(session))
        .with_log_processor(processor)
        .build())
}

/// The resource carries only allowlisted keys (§13.4) plus `service.name`,
/// which OTLP needs to route at all. `builder_empty` is deliberate: the
/// default builder would add `telemetry.sdk.*` attributes nobody asked for.
fn resource(session: &SessionContext) -> Resource {
    let mut attributes = vec![
        KeyValue::new(keys::APP_VERSION, env!("CARGO_PKG_VERSION")),
        KeyValue::new(keys::INSTALL_ID, session.install_id.clone()),
        KeyValue::new(keys::SESSION_ID, session.session_id.clone()),
        KeyValue::new(keys::TERM_GRAPHICS_PROTOCOL, session.graphics_protocol),
        KeyValue::new(keys::TERM_WIDTH_BUCKET, session.width_bucket),
    ];
    if let Some(term) = &session.term_program {
        attributes.push(KeyValue::new(keys::TERM_PROGRAM, term.clone()));
    }

    Resource::builder_empty()
        .with_service_name(APP_DIR)
        .with_attributes(attributes)
        .build()
}

/// OTLP/HTTP wants the signal path on the endpoint; a configured endpoint
/// that already names it is left alone.
fn logs_endpoint(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    if trimmed.ends_with("/v1/logs") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/logs")
    }
}

fn protocol_from(protocol: Option<&str>) -> eyre::Result<Protocol> {
    match protocol.map(str::trim) {
        None | Some("") | Some("http/protobuf") | Some("http-protobuf") | Some("http-binary") => {
            Ok(Protocol::HttpBinary)
        }
        Some("http/json") | Some("http-json") => Ok(Protocol::HttpJson),
        Some(other) => Err(eyre::eyre!(
            "unsupported telemetry protocol {other:?}; use \"http/protobuf\" or \"http/json\""
        )),
    }
}

fn endpoint_set_in_env() -> bool {
    env_present("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT") || env_present("OTEL_EXPORTER_OTLP_ENDPOINT")
}

fn protocol_set_in_env() -> bool {
    env_present("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL") || env_present("OTEL_EXPORTER_OTLP_PROTOCOL")
}

fn env_protocol_is_grpc() -> bool {
    [
        "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
        "OTEL_EXPORTER_OTLP_PROTOCOL",
    ]
    .iter()
    .find_map(|var| std::env::var(var).ok().filter(|v| !v.is_empty()))
    .is_some_and(|value| value.trim() == "grpc")
}

fn env_present(var: &str) -> bool {
    std::env::var_os(var).is_some_and(|value| !value.is_empty())
}

fn mode_str(mode: TelemetryMode) -> &'static str {
    match mode {
        TelemetryMode::Vendor => "vendor",
        TelemetryMode::Custom => "custom",
        TelemetryMode::Off => "off",
    }
}

/// Reads the per-install identity from the config directory, generating and
/// persisting whichever half is missing. Both files are `0600`: the salt is
/// the reason `chat.hash` is irreversible, and it must never leave the
/// machine (spec §13.4).
pub fn load_or_create_identity() -> eyre::Result<Identity> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let id_path = dir.join(INSTALL_ID_FILE);
    let install_id = match std::fs::read_to_string(&id_path) {
        Ok(text) if is_hex_id(text.trim()) => text.trim().to_string(),
        Ok(_) | Err(_) => {
            let id = random_hex(16);
            write_private(&id_path, id.as_bytes())?;
            id
        }
    };

    let salt_path = dir.join(SALT_FILE);
    let salt = match std::fs::read(&salt_path) {
        Ok(bytes) if bytes.len() == 32 => {
            let mut salt = [0u8; 32];
            salt.copy_from_slice(&bytes);
            salt
        }
        Ok(_) | Err(_) => {
            let mut salt = [0u8; 32];
            rand::fill(&mut salt);
            write_private(&salt_path, &salt)?;
            salt
        }
    };

    Ok(Identity { install_id, salt })
}

/// A fresh identifier for this run. Never persisted (spec §13.4).
pub fn new_session_id() -> String {
    random_hex(8)
}

fn config_dir() -> eyre::Result<PathBuf> {
    let strategy = etcetera::choose_base_strategy()
        .map_err(|err| eyre::eyre!("could not determine the config directory: {err}"))?;
    Ok(strategy.config_dir().join(APP_DIR))
}

/// Creates (or truncates) `path` with mode `0600` and writes `bytes`. The
/// mode is set at `open` time rather than afterwards so the content is never
/// world-readable, not even briefly.
fn write_private(path: &std::path::Path, bytes: &[u8]) -> eyre::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::fill(buf.as_mut_slice());
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn is_hex_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use tgt_core::telemetry::TelemetryEvent;
    use tgt_core::telemetry::schema::actions;
    use tracing_subscriber::prelude::*;

    use super::*;

    /// Stands in for the exporter: records the fields of every event that
    /// gets past `PublicOnly`.
    #[derive(Clone, Default)]
    struct CaptureLayer {
        events: Arc<Mutex<Vec<CapturedEvent>>>,
    }

    #[derive(Debug)]
    struct CapturedEvent {
        target: String,
        fields: Vec<String>,
    }

    impl<S: Subscriber> Layer<S> for CaptureLayer {
        fn on_event(&self, event: &Event<'_>, _ctx: LayerContext<'_, S>) {
            let metadata = event.metadata();
            self.events.lock().unwrap().push(CapturedEvent {
                target: metadata.target().to_string(),
                fields: metadata
                    .fields()
                    .iter()
                    .map(|f| f.name().to_string())
                    .collect(),
            });
        }
    }

    #[test]
    fn raw_tracing_event_does_not_reach_export_layer() {
        let capture = CaptureLayer::default();
        let events = capture.events.clone();

        // Scoped to this thread rather than global: the exporter's filter is
        // what is under test, and `logging`'s test needs the global slot.
        // `tracing`'s own `set_default` is deliberate — the one on
        // `SubscriberInitExt` also installs the `log` bridge process-wide,
        // which would make `logging::init`'s `try_init` fail afterwards.
        let _guard = tracing::subscriber::set_default(
            tracing_subscriber::registry().with(PublicOnly::new(capture)),
        );

        tracing::info!("chat {}", "TITLE");
        tracing::info!(target: "tgt_telemetry", chat_title = "TITLE", "impostor on the right target, without the marker");
        tgt_core::emit!(TelemetryEvent::ok(actions::CHAT_OPEN));

        let captured = events.lock().unwrap();
        assert_eq!(
            captured.len(),
            1,
            "only the emit! event may reach the exporter, got {captured:?}"
        );
        assert_eq!(captured[0].target, TELEMETRY_TARGET);
        assert!(
            captured[0].fields.iter().any(|f| f == keys::PUBLIC_MARKER),
            "the exported event must carry the public marker, got {:?}",
            captured[0].fields
        );
        assert!(
            !captured[0].fields.iter().any(|f| f == "message"),
            "emit! must not carry a formatted message, got {:?}",
            captured[0].fields
        );
    }

    #[test]
    fn shutdown_completes_within_two_seconds_against_black_hole_endpoint() {
        // A listener that accepts nothing: the connection sits in the
        // backlog and the export hangs until its own 10 s timeout, which is
        // well past the 2 s the guard is allowed to take.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a black-hole port");
        let endpoint = format!("http://{}", listener.local_addr().unwrap());

        let exporter = init(
            TelemetryMode::Custom,
            &SessionContext {
                install_id: "0123456789abcdef".to_string(),
                session_id: "fedcba98".to_string(),
                term_program: None,
                graphics_protocol: "none",
                width_bucket: "80-120",
            },
            Some(CustomEndpoint {
                endpoint,
                protocol: None,
                headers: Vec::new(),
            }),
        )
        .expect("the exporter builds against any syntactically valid endpoint")
        .expect("custom mode with an endpoint yields an exporter");

        let Exporter { layer, guard } = exporter;
        {
            let _subscriber =
                tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
            tgt_core::emit!(TelemetryEvent::ok(actions::APP_START));
        }

        let started = Instant::now();
        guard.shutdown();
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_millis(2_500),
            "shutdown took {elapsed:?}, past the 2 s ceiling of spec §13.7"
        );
    }

    #[test]
    fn export_request_carries_the_emit_event_and_not_the_raw_one() {
        // A collector reduced to its essentials: accept one request, read it,
        // answer 200. Proves the pipeline actually serializes and sends —
        // the black-hole test above only proves it gives up in time. T52
        // does this properly, decoding protobuf against the full allowlist.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a collector port");
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let collector = std::thread::spawn(move || read_one_request(&listener));

        let exporter = init(
            TelemetryMode::Custom,
            &SessionContext {
                install_id: "0123456789abcdef".to_string(),
                session_id: "fedcba98".to_string(),
                term_program: None,
                graphics_protocol: "none",
                width_bucket: "80-120",
            },
            Some(CustomEndpoint {
                endpoint,
                protocol: None,
                headers: vec![("x-scope-orgid".to_string(), "tgt".to_string())],
            }),
        )
        .expect("the exporter builds")
        .expect("custom mode with an endpoint yields an exporter");

        let Exporter { layer, guard } = exporter;
        {
            let _subscriber =
                tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
            tracing::info!("chat {}", "SECRETTITLE");
            tgt_core::emit!(TelemetryEvent::ok(actions::CHAT_OPEN).with_chat_kind("private"));
        }

        let started = Instant::now();
        guard.shutdown();
        assert!(
            started.elapsed() < Duration::from_millis(2_500),
            "a responsive collector must not push shutdown past its ceiling"
        );

        let request = collector.join().expect("the collector thread finished");
        let request = String::from_utf8_lossy(&request);

        assert!(
            request.contains(&format!("{CLIENT_HEADER}: {}", env!("CARGO_PKG_VERSION"))),
            "the ingest proxy's client header must be on the request"
        );
        assert!(
            request.contains("x-scope-orgid: tgt"),
            "configured custom headers must be on the request"
        );
        for expected in ["chat.open", "private", "install.id", "0123456789abcdef"] {
            assert!(
                request.contains(expected),
                "expected {expected:?} in the exported payload"
            );
        }
        assert!(
            !request.contains("SECRETTITLE"),
            "a raw tracing event reached the wire"
        );
    }

    /// Accepts one connection, reads a whole HTTP request (headers, then
    /// `Content-Length` bytes), answers `200`, and returns the raw bytes.
    fn read_one_request(listener: &std::net::TcpListener) -> Vec<u8> {
        use std::io::Read;

        let (mut stream, _) = listener.accept().expect("a connection from the exporter");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a read deadline so a stalled test fails rather than hangs");

        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let headers_end = find(&request, b"\r\n\r\n");
            if let Some(end) = headers_end {
                let head = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let length: usize = head
                    .split("content-length:")
                    .nth(1)
                    .and_then(|rest| rest.split("\r\n").next())
                    .and_then(|value| value.trim().parse().ok())
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => request.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }

        let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        request
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[test]
    fn vendor_mode_is_inert_without_a_build_time_endpoint() {
        // This build has no TGT_INGEST_ENDPOINT (CI never sets it), so
        // vendor mode must produce nothing at all rather than defaulting to
        // localhost:4318, which is what the SDK would do on its own.
        assert!(VENDOR_ENDPOINT.is_none(), "test build must be vendor-inert");
        assert!(destination(TelemetryMode::Vendor, None).is_none());
        assert!(destination(TelemetryMode::Off, None).is_none());
    }

    #[test]
    fn custom_endpoint_replaces_vendor_and_is_never_combined() {
        let custom = destination(
            TelemetryMode::Custom,
            Some(CustomEndpoint {
                endpoint: "https://collector.example/".to_string(),
                protocol: Some("http/json".to_string()),
                headers: vec![("x-scope-orgid".to_string(), "42".to_string())],
            }),
        )
        .expect("a custom endpoint is a destination");

        assert_eq!(custom.endpoint, "https://collector.example/");
        assert_eq!(
            logs_endpoint(&custom.endpoint),
            "https://collector.example/v1/logs"
        );
        assert_eq!(
            logs_endpoint("https://c.example/v1/logs"),
            "https://c.example/v1/logs"
        );
        assert!(matches!(
            protocol_from(custom.protocol.as_deref()),
            Ok(Protocol::HttpJson)
        ));
        assert!(matches!(protocol_from(None), Ok(Protocol::HttpBinary)));
        assert!(protocol_from(Some("carrier-pigeon")).is_err());
    }

    #[test]
    fn identity_is_stable_across_calls_and_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let _lock = crate::logging::tests::env_lock();
        // SAFETY: serialized against every other test that touches the
        // process environment by the shared lock above.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let first = load_or_create_identity().expect("identity is created on first run");
        let second = load_or_create_identity().expect("identity is read back on later runs");

        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
        }

        assert_eq!(first.install_id, second.install_id);
        assert_eq!(first.salt, second.salt);
        assert_eq!(first.install_id.len(), 32, "16 random bytes as hex");
        assert_ne!(first.salt, [0u8; 32], "a zero salt would hash nothing");

        for file in [INSTALL_ID_FILE, SALT_FILE] {
            let path = tmp.path().join(APP_DIR).join(file);
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "{file} must not be readable by others");
        }
    }

    #[test]
    fn session_ids_differ_between_runs() {
        assert_ne!(new_session_id(), new_session_id());
    }
}
