---
title: Configuration
createTime: 2026/07/31 10:00:00
---

One TOML file, one location, no search chain.

```text
$XDG_CONFIG_HOME/telegram-tui/config.toml
```

defaulting to `~/.config/telegram-tui/config.toml`. There is no `--config` flag, no `./config.toml`, and nothing under `/etc`. If the file doesn't exist, the first run writes a commented template there and continues with the defaults.

## Precedence

Lowest to highest:

1. Compiled-in defaults
2. The config file, per key. A missing key keeps its default rather than resetting the section.
3. Environment overrides, applied after the file so they always win
4. `DO_NOT_TRACK`, which beats `TELEGRAM_TUI_TELEMETRY`
5. `--no-telemetry`, which forces telemetry off for the run without rewriting the file

The last two are master switches, so they silence crash reports and OTLP export together.

## Unknown keys warn, they don't fail

An unrecognised section, or an unrecognised key inside a known section, produces a warning in the log and is ignored. A config written by a newer build won't brick an older one. (`[telemetry.headers]` is exempt from the check, since it's a free-form map.)

A key with the *wrong type* is a hard error, with a message naming the key: `[app].mouse must be a boolean`. Unrecognised enum values are soft. The retired `[telemetry].mode` is the one you'll still meet in an older file: `off` carries across as `enabled = false`, `vendor` and `custom` as `enabled = true`, both with a deprecation warning naming the keys that replaced it. A `mode` value that's none of those warns and is ignored outright, with no fallback. When a file sets both `mode` and `enabled`, `enabled` wins.

## Saving

When the client writes the config (a theme toggle, the credentials wizard, the consent answer), it re-renders the whole template from the current values into a temporary file and renames it into place. Comments are regenerated from the template rather than preserved, so hand-written comments in your file will be lost the first time something saves.

Only four fields can be written from inside the app: theme, the telemetry master switch (`[telemetry] enabled`), credentials, and the consent acknowledgement. Everything else is hand-edit only.

## `[app]`

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `theme` | string | `"default"` | Theme name. See [Themes](../guides/themes.md). Never validated at parse time: an unknown name silently falls back to `default-dark` with a log warning. |
| `layout_breakpoint_cols` | integer | `100` | Terminal width at or above which the two-pane layout is used. Below it, a single-pane stack. Out of `u16` range is a hard error. |
| `mouse` | boolean | `true` | Enables mouse capture at startup. With it on, hold <kbd>Shift</kbd> for native terminal text selection. |
| `inline_images` | boolean | `true` | When false, photos always render as a one-line card regardless of terminal support. |
| `auto_download_photos` | boolean | `true` | Photos in view download automatically. Video, audio and documents never do, regardless of this setting. |

## `[keys]`

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `palette` | string | `"ctrl+p"` | The command palette binding. Accepts `ctrl+<char>` or a single bare character. Anything else warns and falls back to `ctrl+p`. |

::: warning Only one binding is configurable
The internal binding table has three entries (palette, help, quit), but only `palette` is accepted from config. Writing `[keys] help = "F1"` produces an "unknown key" warning and is ignored; <kbd>?</kbd> and <kbd>ctrl</kbd>+<kbd>c</kbd> stay hard-coded. The section is half-built.
:::

## `[telemetry]`

Two different egresses share this section and they don't carry the same guarantee. Crash reports go to the project's Sentry and are on unless you turn them off; the OTLP export goes to a collector you name yourself and does nothing until you name one, since the project operates no OTLP destination.

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `enabled` | boolean | `true` | Master switch over both egresses. `false` is what `--no-telemetry`, `TELEGRAM_TUI_TELEMETRY=off`, `DO_NOT_TRACK` and a Disable at the first-run screen all resolve to. |
| `crash_reports` | boolean | `true` | Sentry crash and error reports: a stack trace, the panic or error message and its cause chain, the app/OS/arch version, and recent actions as breadcrumbs. Setting it to `false` switches off Sentry alone and leaves a configured collector exporting. |
| `endpoint` | string | unset | OTLP base URL for a collector you run. `/v1/logs` is appended if absent. Unset means no OTLP export at all; there's no fallback to `localhost:4318`. |
| `protocol` | string | unset | `http/protobuf` (also `http-protobuf`, `http-binary`, or empty) or `http/json`. gRPC isn't compiled in. Anything else yields no exporter. |
| `headers.<NAME>` | table of strings | empty | Extra HTTP headers on the OTLP request, appended after the always-present `x-tgt-client`. |
| `mode` | string | retired | Still read for old files, then dropped from the file on the next save. See the enum note above. |

::: warning A crash report's contents are not allowlisted
The OTLP path exports allowlisted attribute keys and nothing else, and `crates/app/tests/telemetry_allowlist.rs` decodes the wire in CI to prove it. That test does not cover crash reports, and the allowlist doesn't govern them either: the error message in a report is written by whatever failed rather than picked from a fixed list, so it can carry limited content such as a file path. `send_default_pii: false` plus a `before_send` hook that nulls `server_name` keep your IP address, username and hostname out of it, and `install.id` is deliberately not attached, so a crash can't be joined to a usage session. Breadcrumbs are the exception: they're built from the same allowlisted events the OTLP path exports and carry no more than it does.
:::

A build with no `TGT_SENTRY_DSN` compiled in never calls `sentry::init`, so `crash_reports = true` in such a build installs no panic hook and uploads nothing. Every build from source is one of those, and `tgt telemetry show` says so on the crash-reports line.

Note also that the "environment wins over config" rule holds for `OTEL_EXPORTER_OTLP_ENDPOINT` and `OTEL_EXPORTER_OTLP_PROTOCOL` but not for headers: `[telemetry.headers]` is applied even when `OTEL_EXPORTER_OTLP_HEADERS` is set.

## `[credentials]`

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `api_id` | integer | unset | Your Telegram API id. Must fit an `i32`. |
| `api_hash` | string | unset | Your Telegram API hash. |

The section is only written out once at least one of the two is set, so a freshly generated config doesn't document these at all. See [API credentials](../getting-started/api-credentials.md).

## `[consent]`

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `acknowledged` | boolean | `false` | Whether the first-run telemetry screen has been answered. While false, that screen is shown and neither egress is constructed. Answering it writes this key and `[telemetry].enabled` together, so a Disable there persists as `enabled = false`. |

## A full example

```toml
[app]
theme = "catppuccin-mocha"
layout_breakpoint_cols = 100
mouse = true
inline_images = true
auto_download_photos = true

[keys]
palette = "ctrl+p"

[telemetry]
enabled = true
crash_reports = false
endpoint = "https://otlp.example.com"
protocol = "http/protobuf"

[telemetry.headers]
Authorization = "Basic aGVsbG86dGhlcmU="

[credentials]
api_id = 1234567
api_hash = "0123456789abcdef0123456789abcdef"

[consent]
acknowledged = true
```

The `[telemetry]` block there is a deliberate mix rather than the default: crash reports off, usage export on and pointed at a collector of your own. Leave both keys out and you get the opposite, which is crash reports on and no OTLP export.

## Environment variables

### Credentials and telemetry

| Variable | Effect |
| --- | --- |
| `TELEGRAM_API_ID` | Overrides `credentials.api_id` for the run. Not persisted. A non-integer value warns and is ignored. |
| `TELEGRAM_API_HASH` | Overrides `credentials.api_hash` for the run. Not persisted. |
| `TELEGRAM_TUI_TELEMETRY` | `on`, `off`, `true`, `false`, `1` or `0`, case-insensitive, plus the legacy `vendor` and `custom` which both count as on. Overrides the file for both egresses. Anything else warns and is ignored. |
| `DO_NOT_TRACK` | Any value other than empty or `0` forces telemetry off, beating both the file and `TELEGRAM_TUI_TELEMETRY`. Crash reports and OTLP export alike. |
| `OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT` | When set, the client withholds its own endpoint and lets the OpenTelemetry SDK resolve it. |
| `OTEL_EXPORTER_OTLP_PROTOCOL`, `OTEL_EXPORTER_OTLP_LOGS_PROTOCOL` | Same withholding. A value of `grpc` yields no exporter with an explanatory log line. |

### Terminal and graphics

| Variable | Effect |
| --- | --- |
| `TMUX` | If set, inline images are disabled unless `TGT_FORCE_GRAPHICS=1`. |
| `TGT_FORCE_GRAPHICS` | Exactly `1` overrides the tmux veto. `true` doesn't count. |
| `TERM` | `xterm-kitty`, or containing `ghostty`, selects the kitty protocol. |
| `KITTY_WINDOW_ID` | Set to anything selects the kitty protocol. |
| `TERM_PROGRAM` | `ghostty` selects kitty; `iTerm.app` or `WezTerm` selects the iTerm2 protocol. Also used for the terminal-notification method and reported as the `term.program` telemetry attribute. |
| `TGT_SIXEL` | Exactly `1` selects sixel. Never guessed. |
| `COLORTERM` | Containing `truecolor` or `24bit` enables truecolor; otherwise themes degrade to 256 colours. |

### Paths and runtime

| Variable | Effect |
| --- | --- |
| `XDG_CONFIG_HOME` | Config file, themes directory, install id, telemetry salt. Default `~/.config`. |
| `XDG_STATE_HOME` | Application log and TDLib's log. Default `~/.local/state`. |
| `XDG_DATA_HOME` | TDLib database directory (created mode `0700` on Unix). Default `~/.local/share`. |
| `HOME` | Tilde expansion for `/send ~/...` paths. |
| `TGT_OPENER` | Command used to open a downloaded file. Default `open`. |
| `RUST_LOG` | Filters the local log file only. Default `info`. It deliberately cannot silence telemetry. |
| `TGT_PREFIX` | Used by `mise run install` / `uninstall` only. Default `~/.local`. Not read by the binary. |
| `TGT_SENTRY_DSN` | Build-time only. Bakes in the Sentry DSN for crash reports. Without it the binary never initialises Sentry, so it has no panic hook and no uploader, which is the case for every from-source and CI build. |

## Files on disk

| Path | Mode | What |
| --- | --- | --- |
| `~/.config/telegram-tui/config.toml` | default | This file |
| `~/.config/telegram-tui/themes/<name>.toml` | default | Custom themes |
| `~/.config/telegram-tui/install-id` | `0600` on Unix | Pseudonymous install id |
| `~/.config/telegram-tui/telemetry-salt` | `0600` on Unix | HMAC salt for chat hashes, never transmitted |
| `~/.local/share/telegram-tui/td/` | `0700` on Unix | Encrypted TDLib database |
| `~/.local/state/telegram-tui/tgt.log.<date>` | default | Daily rolling application log |
| `~/.local/state/telegram-tui/tdlib.log` | default | TDLib's own log |

The `0700` and `0600` modes are Unix-only in the current code. On Windows those files inherit the parent directory's ACLs, which is a known gap rather than a decision. The TDLib database encryption key lives in the OS credential store under service `telegram-tui`, entry `db-encryption-key`, not on disk.
