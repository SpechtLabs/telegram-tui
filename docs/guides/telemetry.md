---
title: Telemetry controls
createTime: 2026/07/31 10:00:00
---

`tgt` has two ways of sending diagnostics off your machine, and they don't carry the same promise. Crash reporting is on unless you turn it off; exporting usage data to an OpenTelemetry collector is off unless you point it at one. You're told about both before you log in. This page is the practical side: what each one sends, how to look at it, and how to switch it off. [Telemetry by construction](../understanding/telemetry-allowlist.md) explains why the second one can't leak, and is careful about the fact that the first one has no equivalent proof.

## The two paths at a glance

| | Crash reports | OTLP export |
| --- | --- | --- |
| Default | **On** | **Off** |
| Goes to | the telegram-tui project's Sentry | a collector you name |
| Sent when | the app panics or exits with an error | every action, batched |
| Contents governed by | the failure's own stack and message | a fixed allowlist of 16 attribute keys |
| Proven in CI | no | yes, by decoding the wire |

Both are switched off together by the controls below. The project runs no OTLP destination of its own, so if you don't configure a collector, nothing goes down that path at all.

## What a crash report contains

A report gets built when the app panics, or when it exits with an error. It carries a stack trace, the app and OS version and architecture, the panic or error message with its cause chain, and the last few actions as breadcrumbs.

Your IP address and username aren't attached (`send_default_pii` is off), and neither is your computer's name — a `before_send` hook nulls the hostname field Sentry would otherwise fill, since on a laptop that's usually a person's name. The pseudonymous `install.id` the OTLP path uses isn't attached either, deliberately: giving both egresses a shared key would let a crash be joined to a usage session, and keeping them unlinkable is worth more.

The breadcrumbs are the same allowlisted events described below, so they carry nothing the OTLP path wouldn't.

That leaves the error message, which is the honest caveat. It's written by whatever code failed rather than chosen from a list, so it can carry limited content — a file path you tried to send, a TDLib error string. Nothing in this client formats a chat title or a message body into an error, so in practice that's what you get. But "in practice" is a weaker claim than the allowlist's, and this page won't pretend otherwise.

If you built from source, none of this applies to your binary: the Sentry DSN is baked in at compile time from `TGT_SENTRY_DSN`, and without it the client never initialises Sentry at all. No panic hook, no uploader, nothing to send to. The first-run screen changes its wording to match, rather than offering to turn on something that isn't there, and `tgt telemetry show` says the same. If you maintain a build like that and want its crashes to reach the project, export the variable before building — see [contributing](../understanding/contributing.md).

## What the OTLP path sends

Sixteen attribute keys exist in total, and that list is the complete set. Session-constant values ride on the OTLP resource rather than being repeated on every record:

`app.version`, `os.version`, `term.program`, `term.graphics_protocol`, `term.width_bucket`, `install.id`, `session.id`.

Per-event: `action`, `outcome`, `error.kind`, `duration_ms`, `chat.kind`, `chat.hash`, `history.page_depth`, `download.size_bucket`, plus the `telemetry.public` marker.

Every one of those is either a compile-time constant, a number, or a bucket. `term.width_bucket` is one of `<80`, `80-120`, `120-160`, `>160`. `outcome` is `ok`, `error`, or `cancelled`. `chat.kind` is `private`, `group`, `supergroup`, or `channel`. `chat.hash` is an HMAC-SHA256 of the chat id under a salt generated locally and never transmitted, truncated to 8 bytes: stable within your install, uncorrelatable across installs, irreversible.

Actions are drawn from a closed set of 17 names like `chat.open`, `message.send`, `message.reply`, `search.run`. Error kinds from a closed set of 9 like `td.flood_wait`, `net.timeout`.

Not on the list, and therefore not expressible on this path: message text, contact names, usernames, phone numbers, chat titles, file names, search queries. A search emits `search.run` and the kind of chat it ran in; the query never leaves the process.

## Seeing what would be sent

```shell
tgt telemetry show
```

It prints whether telemetry is on, the state of each egress, what a crash report is made of, then every resource attribute with its live value and every event attribute name with its allowed values. For the OTLP half that really is the complete list. For crash reports it can only describe the shape, because a report gets assembled out of a failure that hasn't happened yet.

It never starts the TUI, and it respects `--no-telemetry`, so `tgt --no-telemetry telemetry show` reports everything off.

## Turning it off

Any one of these switches off **both** paths:

| Method | Scope |
| --- | --- |
| Answer **Disable** on the first-run screen | Persistent, saved as `enabled = false` |
| `[telemetry] enabled = false` in the config | Persistent |
| `TELEGRAM_TUI_TELEMETRY=off` | That environment |
| `DO_NOT_TRACK=1` | That environment, beats the config |
| `tgt --no-telemetry` | That run only, config untouched |

`DO_NOT_TRACK` counts as set for any value other than empty or `0`, honouring the [consensus](https://consoledonottrack.com/) convention.

To keep one and drop the other, `[telemetry] crash_reports = false` silences Sentry while leaving a configured collector running, and simply not setting `endpoint` — the default — means no OTLP export while crash reports continue.

With telemetry off, nothing is constructed: `sentry::init` is never called, so there's no panic hook and no uploader, and no OTLP exporter is built either. The events still fire internally and land in the local rolling log; there's just nothing on the other end of them.

## Pointing it at your own collector

```toml
[telemetry]
enabled = true
endpoint = "https://otlp.example.com"
protocol = "http/protobuf"    # or "http/json"

[telemetry.headers]
x-scope-orgid = "my-tenant"
```

`/v1/logs` is appended to the endpoint if it isn't already there. gRPC isn't compiled in, and asking for it produces no exporter rather than an error at startup. With no `endpoint` set the client exports nowhere rather than falling back to `localhost:4318` the way the OpenTelemetry SDK would on its own.

If you set `OTEL_EXPORTER_OTLP_ENDPOINT` or `OTEL_EXPORTER_OTLP_PROTOCOL` (or their `_LOGS_` variants), the client withholds its own programmatic values so the SDK resolves them, and your environment wins. That withholding is not implemented for headers, so `[telemetry.headers]` is applied even when `OTEL_EXPORTER_OTLP_HEADERS` is set. That's an inconsistency rather than a design.

::: tip Upgrading from an older config
The old `mode = "vendor" | "custom" | "off"` key still loads. `off` carries across as `enabled = false`, and the other two as `enabled = true`, each with a warning in the log naming the keys that replaced it. An opt-out you wrote once won't be quietly upgraded into telemetry being back on.
:::

## The pseudonymous install id

Two files under `~/.config/telegram-tui/`, both mode `0600` on Unix: `install-id` (the id itself) and `telemetry-salt` (32 random bytes used to hash chat ids). The salt is never printed and never transmitted.

```shell
tgt telemetry reset-id
```

regenerates both and prints the old and new install id. Regenerating the salt means every `chat.hash` your install produces changes, which is the point: past and future events can't be joined.

On Windows those files inherit the parent directory's ACLs instead of getting the `0600` treatment. That hardening is Unix-only in the current code and unfinished.

## Runtime behaviour

The export queue is bounded at 512 records and drops when full rather than blocking, batches flush every 5 seconds, and shutdown has a hard 2-second ceiling on each path. An export failure produces one debug log line and nothing else. A collector you can't reach is never a reason to refuse to run a chat client, so a failing telemetry init is treated as "no telemetry", not as a startup error.

The crash reporter is set up after the error handler and before the panic hook that restores your terminal, so a panic puts the shell back first, then captures, then prints. Uploading a report over a frozen alternate screen would be the one way crash reporting could make a crash worse.

## The local log stays rich

`~/.local/state/telegram-tui/tgt.log.<date>` is a daily rolling file and it holds everything, including the things neither egress carries. That's the file to attach to a bug report; read it first, because it's much more detailed than anything telemetry sends. `RUST_LOG` filters it and deliberately cannot silence telemetry.

Terminal notifications get the same treatment: the alert function takes no content parameters at all, so no sender name or message text can ride an `OSC 777` into a multiplexer's log.
