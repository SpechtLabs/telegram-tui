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

## Unknown keys warn, they don't fail

An unrecognised section, or an unrecognised key inside a known section, produces a warning in the log and is ignored. A config written by a newer build won't brick an older one. (`[telemetry.headers]` is exempt from the check, since it's a free-form map.)

A key with the *wrong type* is a hard error, with a message naming the key: `[app].mouse must be a boolean`. Unrecognised enum values are soft: an unknown `telemetry.mode` warns and falls back to `vendor`.

## Saving

When the client writes the config (a theme toggle, the credentials wizard, the consent answer), it re-renders the whole template from the current values into a temporary file and renames it into place. Comments are regenerated from the template rather than preserved, so hand-written comments in your file will be lost the first time something saves.

Only four fields can be written from inside the app: theme, telemetry mode, credentials, and the consent acknowledgement. Everything else is hand-edit only.

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

| Key | Type | Default | What it does |
| --- | --- | --- | --- |
| `mode` | string | `"vendor"` | `vendor`, `custom`, or `off`, case-insensitive. Unrecognised values warn and use `vendor`. |
| `endpoint` | string | unset | OTLP base URL. **Only read when `mode = "custom"`.** `/v1/logs` is appended if absent. |
| `protocol` | string | unset | `http/protobuf` (also `http-protobuf`, `http-binary`, or empty) or `http/json`. gRPC isn't compiled in. Anything else yields no exporter. **Only read when `mode = "custom"`.** |
| `headers.<NAME>` | table of strings | empty | Extra HTTP headers, appended after the always-present `x-tgt-client`. **Only read when `mode = "custom"`.** |

::: warning endpoint, protocol and headers are silently inert outside custom mode
The generated template renders all three unconditionally, and there is no warning when they're set while the mode is `vendor`. Setting an endpoint without also setting `mode = "custom"` does nothing at all.
:::

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
| `acknowledged` | boolean | `false` | Whether the first-run telemetry screen has been answered. While false, that screen is shown and no exporter is constructed. |

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
mode = "off"

[credentials]
api_id = 1234567
api_hash = "0123456789abcdef0123456789abcdef"

[consent]
acknowledged = true
```

## Environment variables

### Credentials and telemetry

| Variable | Effect |
| --- | --- |
| `TELEGRAM_API_ID` | Overrides `credentials.api_id` for the run. Not persisted. A non-integer value warns and is ignored. |
| `TELEGRAM_API_HASH` | Overrides `credentials.api_hash` for the run. Not persisted. |
| `TELEGRAM_TUI_TELEMETRY` | `vendor`, `custom` or `off`, case-insensitive. Overrides the file. |
| `DO_NOT_TRACK` | Any value other than empty or `0` forces telemetry off, beating both the file and `TELEGRAM_TUI_TELEMETRY`. |
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
| `TGT_INGEST_ENDPOINT` | Build-time only. Bakes in the vendor telemetry destination; a build without it has an inert vendor mode. |

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
