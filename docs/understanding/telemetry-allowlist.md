---
title: Telemetry by construction
createTime: 2026/07/31 10:00:00
---

Most privacy promises are policies: a rule in a contributing guide saying don't log message contents, enforced by whoever reviews the pull request. That works until the first person who forgets, and it fails silently.

`tgt` makes the same promise a property of the type system, the macro system, and a CI gate. A leak isn't something to be careful about; it's something that doesn't compile, or gets dropped at the layer boundary, or turns CI red.

For the practical controls, see [Telemetry controls](../guides/telemetry.md). This page is about why the guarantee holds.

## The claim

> Message text, contact names, usernames, phone numbers, chat titles, file names, and search queries cannot be exported by this client.

Not "aren't", not "shouldn't be". Cannot.

## Layer 1: the event type can't hold free text

```rust
/// Every field is either a schema constant (&'static str) or a number/bucket.
/// Free-form strings are structurally impossible except chat_hash, which is
/// produced only by telemetry::hashing::hash_id.
pub struct TelemetryEvent {
    pub action: &'static str,            // schema::actions::*
    pub outcome: Outcome,
    pub error_kind: Option<&'static str>, // schema::error_kinds::*
    pub duration_ms: Option<u64>,
    pub chat_kind: Option<&'static str>,
    pub chat_hash: Option<String>,
    pub history_page_depth: Option<u32>,
    pub download_size_bucket: Option<&'static str>,
}
```

The `&'static str` typing is the load-bearing decision. Every chat title, message body, username and file name in this program is a runtime-constructed `String`, and a `String` does not coerce to a `&'static str`. You cannot assign a chat title to `action`. The compiler stops you, at the definition site, without anyone having to notice.

The single `String` field is `chat_hash`, and it's annotated with where its values may come from: an HMAC-SHA256 of the chat id under a locally generated 32-byte salt, truncated to 8 bytes and hex-encoded. Stable within an install, uncorrelatable across installs, irreversible. The salt is never transmitted.

## Layer 2: one exit, with a fixed field list

```rust
//! The ONLY path to the OTLP exporter. The subscriber layer in tgt-app
//! exports only events carrying `telemetry.public` AND target
//! `"tgt_telemetry"`; everything else stays in the local rolling file.
#[macro_export]
macro_rules! emit {
    ($event:expr) => {{
        let __ev: $crate::telemetry::TelemetryEvent = $event;
        ::tracing::info!(
            target: "tgt_telemetry",
            action = __ev.action,
            telemetry.public = true,
            ...
```

The macro takes one argument: an event. It has no variadic form. No call site can add an attribute, even a legal one, so the exported attribute set is fixed at compile time by the macro body rather than by whoever is writing the call.

And there's exactly one `emit!` call site in the whole binary, in the effect dispatcher. Events are minted in one place too, the router in `app.rs`, keyed off the TDLib requests a route produced rather than inside the handlers. One place decides what a user action is called, so two handlers can never disagree, and an unconfirmed dialog can't emit an event for the thing it didn't do.

Note what that costs and where the line is: a search emits `search.run` and the kind of chat it ran in, and the query text is never part of the event, because there is no field it could occupy.

## Layer 3: bypassing the macro produces nothing

Suppose someone writes `tracing::info!("opening chat {}", chat.title)` in a hurry.

```rust
pub fn is_public_telemetry(metadata: &Metadata<'_>) -> bool {
    metadata.target() == TELEMETRY_TARGET
        && metadata.fields().field(keys::PUBLIC_MARKER).is_some()
}
```

The exporter layer forwards only events that carry both the `tgt_telemetry` target and the `telemetry.public` field. That stray `info!` has neither, so it's dropped at the layer boundary. It still reaches the local rolling log file, which is the intended sink and never leaves the machine.

There's a detail in the implementation worth pausing on. The check lives in `on_event` rather than in `enabled` or `register_callsite`, and the comment explains why: those are consulted by the layered subscriber for the whole stack, so returning "never" there would also silence the file log, which is the sink that's supposed to see everything. Getting this right meant accepting a slightly slower path in exchange for not blinding the debugging log.

## Layer 4: CI decodes the wire and asserts on the complement

The type system can't reach one place: the OTLP resource, the session-scope attributes assembled by hand in `tgt-app`. That's exactly where the integration proof is pointed.

`crates/app/tests/telemetry_allowlist.rs` boots the real app against the real dispatcher, the real exporter that `otel::init` builds, and an in-process OTLP collector over a real HTTP round trip. It drives a whole session (login, chat open, react, delete, reply, send, edit, palette, search), flushes, and then checks what arrived.

The stub *decodes* the protobuf rather than grepping it, and the reason is precise:

> The allowlist claim is a claim about the *complement*: no key outside `ALLOWED_KEYS`, anywhere. That can only be checked by decoding the payload and enumerating what it holds.

Four assertions, and the interesting ones are not the obvious one:

**Subset.** Every arrived key is in the allowlist, plus a protocol exception list spelled out as a list of exactly one (`service.name`) "so that a second exception has to be added here, in a diff, with a reason."

**Anti-vacuity.** Twelve named keys and nine named actions *must* be present. Without this, an exporter that silently stopped working would satisfy the subset assertion perfectly. This is the assertion that keeps the test from becoming decorative.

**Shape.** Every record's scope is `tgt_telemetry` and every record's body is `None`, because a telemetry event with a formatted message is free-form text by definition.

**Raw-byte search.** For fourteen forbidden strings (both chat ids, the supergroup id, two chat titles, three message bodies, the file name, the phone number, the login code, the search query, the sender's name), a substring search over the raw exported bytes. Not an equality check over decoded values:

> The ids are checked as substrings rather than whole values because a leak does not have to be neat: a chat id concatenated into some other attribute's value would slip past an equality check.

There's also a negative-coverage canary. Six actions currently have no emitter, and the test asserts they're *absent*; growing an emitter for one of them fails the test and forces the documentation table to be updated in the same change.

Two more details show the level of care. Fixed constants are used rather than random ones, because "a flaky privacy test is worse than none". And a supporting test proves the forbidden-string search can't false-negative against the session's own identity values, because the first draft picked a session id ending `…9876543210`, which contains the login code, and failed exactly as it should have.

## The consent gate

Consent is checked before an exporter object is ever constructed:

```rust
let otel_guard = if config.consent_acknowledged && telemetry_mode != TelemetryMode::Off {
```

Since the value is read from disk before the consent screen can run, your answer takes effect on the *next* start. This run exports nothing regardless of what you pick, because there's nothing to export through.

The consent screen swallows every key except quit rather than passing unrecognised ones through, and the code says why: "letting an unrecognized key fall through would be the one crack that leaks a keystroke to whatever screen comes after this one."

The gate condition is replicated in the test, plus a source-text canary that greps `main.rs` for the literal line. The test's own doc block is honest about what it can't prove: a change that moved exporter construction *earlier* than the consent check would break that first part and neither of the others.

## Adding an attribute is a reviewed diff

```rust
#[test]
fn allowlist_snapshot() {
    insta::assert_json_snapshot!(ALLOWED_KEYS);
}
```

Adding a key means adding a constant, which changes the snapshot, which shows up in review as a diff of the privacy contract rather than as a line buried in a hundred-file change.

## What this is not

It isn't a claim that the client can't be made to leak by someone determined to. Someone with commit access can delete the layer filter. The claim is narrower and more useful: no ordinary mistake produces a leak. Adding a log line doesn't. Adding a field to an event doesn't compile. Forgetting the review rule doesn't matter, because there's no review rule to forget.

The same discipline extends past telemetry. The terminal notification function takes no content parameters at all, so no sender name or message text can ride an `OSC 777` escape into a multiplexer's log. The pattern is consistent: make the dangerous thing unsayable, rather than asking people not to say it.
