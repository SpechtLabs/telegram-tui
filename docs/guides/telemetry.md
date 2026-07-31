---
title: Telemetry controls
createTime: 2026/07/31 10:00:00
---

Telemetry is opt-in with disclosure, and you're asked before you log in. This page is the practical side: what's sent, how to see it, and how to switch it off. [Telemetry by construction](../understanding/telemetry-allowlist.md) explains why a PII leak isn't a thing that can happen here.

## What a session sends

Sixteen attribute keys exist in total, and that list is the complete set. Session-constant values ride on the OTLP resource rather than being repeated on every record:

`app.version`, `os.version`, `term.program`, `term.graphics_protocol`, `term.width_bucket`, `install.id`, `session.id`.

Per-event: `action`, `outcome`, `error.kind`, `duration_ms`, `chat.kind`, `chat.hash`, `history.page_depth`, `download.size_bucket`, plus the `telemetry.public` marker.

Every one of those is either a compile-time constant, a number, or a bucket. `term.width_bucket` is one of `<80`, `80-120`, `120-160`, `>160`. `outcome` is `ok`, `error`, or `cancelled`. `chat.kind` is `private`, `group`, `supergroup`, or `channel`. `chat.hash` is an HMAC-SHA256 of the chat id under a salt generated locally and never transmitted, truncated to 8 bytes: stable within your install, uncorrelatable across installs, irreversible.

Actions are drawn from a closed set of 17 names like `chat.open`, `message.send`, `message.reply`, `search.run`. Error kinds from a closed set of 9 like `td.flood_wait`, `net.timeout`.

Not on the list, and therefore not expressible: message text, contact names, usernames, phone numbers, chat titles, file names, search queries. A search emits `search.run` and the kind of chat it ran in; the query never leaves the process.

## Seeing exactly what would be sent

```shell
tgt telemetry show
```

It prints the current mode, the destination, every resource attribute with its live value, and every event attribute name with its allowed values. It never starts the TUI, and it respects `--no-telemetry`, so `tgt --no-telemetry telemetry show` reports mode `off`.

## Turning it off

Any one of these is sufficient:

| Method | Scope |
| --- | --- |
| Answer **Disable** on the first-run screen | Persistent |
| `[telemetry] mode = "off"` in the config | Persistent |
| `TELEGRAM_TUI_TELEMETRY=off` | That environment |
| `DO_NOT_TRACK=1` | That environment, beats everything except the flag |
| `tgt --no-telemetry` | That run only, config untouched |

`DO_NOT_TRACK` counts as set for any value other than empty or `0`, honouring the [consensus](https://consoledonottrack.com/) convention.

With any of them in effect, no exporter is constructed at all. The events still fire internally and land in the local rolling log; there's simply nothing on the other end of them.

## Where it would go

Nowhere, in most builds. The vendor endpoint is baked in at compile time from `TGT_INGEST_ENDPOINT`, and a build without it has an inert vendor mode rather than falling back to `localhost:4318` the way the OpenTelemetry SDK would on its own. If you built from source and didn't set that variable, "vendor" mode sends nothing.

To point it at your own collector:

```toml
[telemetry]
mode = "custom"
endpoint = "https://otlp.example.com"
protocol = "http/protobuf"    # or "http/json"

[telemetry.headers]
x-scope-orgid = "my-tenant"
```

Custom fully replaces vendor; the two are never combined. `/v1/logs` is appended to the endpoint if it isn't already there. gRPC isn't compiled in, and asking for it produces no exporter rather than an error at startup.

::: warning endpoint, protocol and headers do nothing unless mode is "custom"
The generated config file renders all three keys unconditionally, and setting an endpoint while the mode is `vendor` (the default) is silently inert. There's no warning for it. This is a rough edge in the current build.
:::

If you set `OTEL_EXPORTER_OTLP_ENDPOINT` or `OTEL_EXPORTER_OTLP_PROTOCOL` (or their `_LOGS_` variants), the client withholds its own programmatic values so the SDK resolves them, and your environment wins. That withholding is not implemented for headers, so `[telemetry.headers]` is applied even when `OTEL_EXPORTER_OTLP_HEADERS` is set. That's an inconsistency rather than a design.

## The pseudonymous install id

Two files under `~/.config/telegram-tui/`, both mode `0600` on Unix: `install-id` (the id itself) and `telemetry-salt` (32 random bytes used to hash chat ids). The salt is never printed and never transmitted.

```shell
tgt telemetry reset-id
```

regenerates both and prints the old and new install id. Regenerating the salt means every `chat.hash` your install produces changes, which is the point: past and future events can't be joined.

On Windows those files inherit the parent directory's ACLs instead of getting the `0600` treatment. That hardening is Unix-only in the current code and unfinished.

## Runtime behaviour

The export queue is bounded at 512 records and drops when full rather than blocking, batches flush every 5 seconds, and shutdown has a hard 2-second ceiling. An export failure produces one debug log line and nothing else. A collector you can't reach is never a reason to refuse to run a chat client, so a failing telemetry init is treated as "no telemetry", not as a startup error.

## The local log stays rich

`~/.local/state/telegram-tui/tgt.log.<date>` is a daily rolling file and it holds everything, including the things that could never be exported. That's the file to attach to a bug report; read it first, because it's much more detailed than anything telemetry carries. `RUST_LOG` filters it and deliberately cannot silence telemetry.

Terminal notifications get the same treatment: the alert function takes no content parameters at all, so no sender name or message text can ride an `OSC 777` into a multiplexer's log.
