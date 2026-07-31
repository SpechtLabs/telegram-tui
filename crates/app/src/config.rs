//! TOML config load/generate (`etcetera` paths), unknown-key warnings,
//! `ConfigPatch` application. See docs/architecture.md §2.3, §4.4, §4.6;
//! spec §12.
//!
//! # Schema
//!
//! The on-disk shape matches spec §12: `[app]` (theme, layout_breakpoint_cols,
//! mouse, inline_images),
//! `[keys]` (palette), `[telemetry]` (mode, optional endpoint/protocol/headers).
//! Two sections extend beyond the spec's illustrative sample so the state
//! `ConfigPatch` can mutate actually persists somewhere: `[credentials]`
//! (api_id/api_hash, written by the auth wizard per spec §9.1 — "writes it
//! to config") and `[consent]` (acknowledged, written on first-run consent).
//! Unknown keys anywhere in the document produce a local `tracing::warn!`
//! rather than a hard failure (spec §12), so a config written by a newer
//! binary doesn't brick an older one.
//!
//! # Unknown-key detection
//!
//! `load()` parses the file into a generic `toml::Table` first and walks it
//! against a hand-maintained list of known sections/keys, warning on
//! anything it doesn't recognize, before extracting typed values field by
//! field (each extraction site produces a contextual error on a type
//! mismatch, e.g. a string where an integer is expected). This is the
//! `toml::Value` round-trip approach rather than `#[serde(deny_unknown_fields)]`
//! precisely because deny-unknown-fields would turn "newer config, older
//! binary" into a hard failure — the opposite of what spec §12 asks for.
//!
//! # Atomic save
//!
//! `save()` writes the fully re-rendered commented template to a temp file
//! in the same directory as `config.toml`, then `rename`s it into place.
//! `rename` within one filesystem is atomic on macOS, so a crash or
//! concurrent read never observes a partially written file. The template is
//! regenerated from the struct's current values on every save (rather than
//! patched in place), which is the simplest way to guarantee the file stays
//! well-formed and keeps its comments after edits made only through
//! `apply_patch`.

use std::path::PathBuf;

use color_eyre::eyre::{self, Context};
use etcetera::BaseStrategy;
use tgt_core::effect::{ConfigPatch, TelemetryMode};
use tgt_core::model::key::{Key, KeyBindings};

const APP_DIR: &str = "telegram-tui";
const CONFIG_FILE: &str = "config.toml";

/// Loaded configuration; carries everything `tgt_core::app::Boot` needs
/// (see `boot_fields`) plus the raw key-binding strings kept around so
/// `save()` can round-trip them verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub theme: String,
    pub layout_breakpoint_cols: u16,
    /// Whether to put the terminal into mouse-reporting mode (architecture
    /// §7.5). Default on. Read once at startup by `main.rs`; never reaches
    /// `tgt-core`, which only ever sees the semantic actions a resolved
    /// click produces.
    pub mouse: bool,
    /// Whether a downloaded photo may render as a picture on a terminal that
    /// supports one (docs/design-language.md §6). Default on. Read once at
    /// startup by `main.rs`, which turns it off by handing `tgt-ui` no
    /// graphics capability at all; like `mouse`, it never reaches
    /// `tgt-core`.
    ///
    /// This is the switch for "my terminal claims a protocol but the result
    /// looks wrong", which no terminal probe can detect on the user's
    /// behalf — over ssh into a host whose terminal type is misreported, or
    /// under a multiplexer the probe does not know about.
    pub inline_images: bool,
    /// Raw `"ctrl+p"`-style string; parsed into a `Key` by `boot_fields`.
    pub palette_key: String,
    pub telemetry_mode: TelemetryMode,
    pub telemetry_endpoint: Option<String>,
    pub telemetry_protocol: Option<String>,
    pub telemetry_headers: Vec<(String, String)>,
    /// Written by `ConfigPatch::Credentials` (spec §9.1); overridden at load
    /// time (not persisted) by `TELEGRAM_API_ID`/`TELEGRAM_API_HASH`.
    pub api_id: Option<i32>,
    pub api_hash: Option<String>,
    /// Written by `ConfigPatch::ConsentAcknowledged`.
    pub consent_acknowledged: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            theme: "default".to_string(),
            layout_breakpoint_cols: 100,
            mouse: true,
            inline_images: true,
            palette_key: "ctrl+p".to_string(),
            telemetry_mode: TelemetryMode::Vendor,
            telemetry_endpoint: None,
            telemetry_protocol: None,
            telemetry_headers: Vec::new(),
            api_id: None,
            api_hash: None,
            consent_acknowledged: false,
        }
    }
}

/// The user-configured destination `mode = "custom"` resolves to (spec
/// §13.5). Shaped like `otel::CustomEndpoint` on purpose — `main.rs` maps
/// one directly into the other — but kept as this module's own type so
/// `config.rs` never has to depend on `otel.rs` (see `Config::custom_destination`).
#[derive(Debug, Clone, PartialEq)]
pub struct CustomDestination {
    pub endpoint: String,
    pub protocol: Option<String>,
    pub headers: Vec<(String, String)>,
}

/// The subset of `Config` that `tgt_core::app::Boot` is built from.
#[derive(Debug, Clone, PartialEq)]
pub struct BootFields {
    pub theme_name: String,
    pub bindings: KeyBindings,
    pub layout_breakpoint_cols: u16,
    pub telemetry_mode: TelemetryMode,
    pub has_credentials: bool,
}

impl Config {
    /// Projects the fields `Boot` needs, parsing the configured key
    /// bindings along the way (`help`/`quit` are not yet configurable and
    /// keep their `KeyBindings::default()` values).
    pub fn boot_fields(&self) -> BootFields {
        let defaults = KeyBindings::default();
        let bindings = KeyBindings {
            palette: parse_key(&self.palette_key, defaults.palette),
            ..defaults
        };
        BootFields {
            theme_name: self.theme.clone(),
            bindings,
            layout_breakpoint_cols: self.layout_breakpoint_cols,
            telemetry_mode: self.telemetry_mode,
            has_credentials: self.api_id.is_some() && self.api_hash.is_some(),
        }
    }

    /// Applies one of the mutations `App::update` may request via
    /// `Effect::SaveConfig`. Does not persist; call `save()` afterward.
    pub fn apply_patch(&mut self, patch: &ConfigPatch) {
        match patch {
            ConfigPatch::Theme(name) => self.theme = name.clone(),
            ConfigPatch::TelemetryMode(mode) => self.telemetry_mode = *mode,
            ConfigPatch::Credentials { api_id, api_hash } => {
                self.api_id = Some(*api_id);
                self.api_hash = Some(api_hash.clone());
            }
            ConfigPatch::ConsentAcknowledged { enabled } => self.consent_acknowledged = *enabled,
        }
    }

    /// The destination this config resolves to under `mode = "custom"`
    /// (spec §13.5). `None` when the mode isn't `custom` at all, or when
    /// custom mode has no endpoint configured — an unset custom destination
    /// is inert the same way an unset vendor one is. This is the single
    /// place that reads the custom-endpoint fields, shared by `main.rs`'s
    /// exporter startup (which maps it into `otel::CustomEndpoint`) and
    /// `telemetry_cli::show`, so the two can never disagree about what
    /// custom mode would send to.
    ///
    /// Deliberately not `otel::CustomEndpoint` itself: several integration
    /// tests under `tests/` pull in `config.rs` via `#[path]` without
    /// `otel.rs` (it drags in the whole OTLP stack), so this module must
    /// stay free of that dependency.
    pub fn custom_destination(&self) -> Option<CustomDestination> {
        if self.telemetry_mode != TelemetryMode::Custom {
            return None;
        }
        let endpoint = self.telemetry_endpoint.clone()?;
        if endpoint.trim().is_empty() {
            return None;
        }
        Some(CustomDestination {
            endpoint,
            protocol: self.telemetry_protocol.clone(),
            headers: self.telemetry_headers.clone(),
        })
    }

    /// Atomically writes the current config to disk as a freshly rendered,
    /// commented TOML document (see module docs).
    pub fn save(&self) -> eyre::Result<()> {
        let path = config_path()?;
        let parent = path
            .parent()
            .ok_or_else(|| eyre::eyre!("config path {} has no parent directory", path.display()))?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;

        // Unique-enough per process; concurrent saves from the same process
        // to the same config file are not a case this app produces (one
        // `App`, one dispatcher), so a pid-scoped name is sufficient to
        // avoid colliding with a previous run's leftover temp file.
        let tmp_path = parent.join(format!(".{CONFIG_FILE}.tmp-{}", std::process::id()));
        std::fs::write(&tmp_path, self.render())
            .with_context(|| format!("failed to write {}", tmp_path.display()))?;
        std::fs::rename(&tmp_path, &path).with_context(|| {
            format!(
                "failed to move {} into place at {}",
                tmp_path.display(),
                path.display()
            )
        })?;
        Ok(())
    }

    /// Renders the current values as the commented TOML document written on
    /// first run and on every `save()`.
    fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("# telegram-tui configuration\n");
        out.push_str("# Generated by telegram-tui; edit freely. Unknown keys produce a warning\n");
        out.push_str("# in the log rather than a hard failure, so a config written by a newer\n");
        out.push_str("# version of the app won't brick an older binary. See spec §12.\n\n");

        out.push_str("[app]\n");
        out.push_str(&format!("theme = {}\n", toml_string(&self.theme)));
        out.push_str(&format!(
            "layout_breakpoint_cols = {}\n",
            self.layout_breakpoint_cols
        ));
        out.push_str("# Mouse reporting: click a chat, a folder tab or the composer, and scroll\n");
        out.push_str("# either pane with the wheel. While it is on, the terminal's own text\n");
        out.push_str("# selection needs shift held down — set this to false to get it back.\n");
        out.push_str(&format!("mouse = {}\n", self.mouse));
        out.push_str("# Inline images: render a downloaded photo as a picture where the\n");
        out.push_str("# terminal speaks kitty, iTerm2 or sixel. Off, or on a terminal without\n");
        out.push_str("# one, a photo shows as a single descriptive line instead. Inside tmux\n");
        out.push_str("# images stay off unless TGT_FORCE_GRAPHICS=1 says passthrough is set up.\n");
        out.push_str(&format!("inline_images = {}\n\n", self.inline_images));

        out.push_str("[keys]\n");
        out.push_str("# Global command-palette shortcut, e.g. \"ctrl+p\" or a bare character.\n");
        out.push_str(&format!("palette = {}\n\n", toml_string(&self.palette_key)));

        out.push_str("[telemetry]\n");
        out.push_str(&format!(
            "mode = {}        # \"vendor\" | \"custom\" | \"off\"\n",
            toml_string(telemetry_mode_str(self.telemetry_mode))
        ));
        match &self.telemetry_endpoint {
            Some(v) => out.push_str(&format!("endpoint = {}\n", toml_string(v))),
            None => out.push_str("# endpoint = \"https://otlp.example.com\"\n"),
        }
        match &self.telemetry_protocol {
            Some(v) => out.push_str(&format!("protocol = {}\n", toml_string(v))),
            None => out.push_str("# protocol = \"http/protobuf\"\n"),
        }
        if self.telemetry_headers.is_empty() {
            out.push_str("# [telemetry.headers]\n# Authorization = \"Basic …\"\n");
        } else {
            out.push_str("\n[telemetry.headers]\n");
            for (k, v) in &self.telemetry_headers {
                out.push_str(&format!("{k} = {}\n", toml_string(v)));
            }
        }

        if self.api_id.is_some() || self.api_hash.is_some() {
            out.push_str("\n[credentials]\n");
            out.push_str("# Written by the auth wizard (spec §9.1). TELEGRAM_API_ID and\n");
            out.push_str(
                "# TELEGRAM_API_HASH override these at load time without touching this file.\n",
            );
            if let Some(id) = self.api_id {
                out.push_str(&format!("api_id = {id}\n"));
            }
            if let Some(hash) = &self.api_hash {
                out.push_str(&format!("api_hash = {}\n", toml_string(hash)));
            }
        }

        out.push_str("\n[consent]\n");
        out.push_str(&format!("acknowledged = {}\n", self.consent_acknowledged));

        out
    }
}

/// Renders `s` as a valid, properly escaped TOML string literal.
fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

fn telemetry_mode_str(mode: TelemetryMode) -> &'static str {
    match mode {
        TelemetryMode::Vendor => "vendor",
        TelemetryMode::Custom => "custom",
        TelemetryMode::Off => "off",
    }
}

fn parse_telemetry_mode(s: &str) -> Option<TelemetryMode> {
    match s.to_ascii_lowercase().as_str() {
        "vendor" => Some(TelemetryMode::Vendor),
        "custom" => Some(TelemetryMode::Custom),
        "off" => Some(TelemetryMode::Off),
        _ => None,
    }
}

/// Parses a rebindable-key string ("ctrl+p" → `Key::Ctrl('p')`, "?" →
/// `Key::Char('?')`). Anything else warns and falls back to `default`.
fn parse_key(s: &str, default: Key) -> Key {
    let trimmed = s.trim();
    if let Some(rest) = trimmed
        .strip_prefix("ctrl+")
        .or_else(|| trimmed.strip_prefix("Ctrl+"))
    {
        let mut chars = rest.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Key::Ctrl(c.to_ascii_lowercase());
        }
    } else {
        let mut chars = trimmed.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return Key::Char(c);
        }
    }
    tracing::warn!(value = %s, "unrecognized key binding in config; using default");
    default
}

/// `$XDG_CONFIG_HOME/telegram-tui/config.toml`, defaulting to
/// `~/.config/telegram-tui/config.toml` (spec §12).
fn config_path() -> eyre::Result<PathBuf> {
    let strategy = etcetera::choose_base_strategy()
        .map_err(|err| eyre::eyre!("could not determine the config directory: {err}"))?;
    Ok(strategy.config_dir().join(APP_DIR).join(CONFIG_FILE))
}

/// Loads the config, generating the commented default file on first run.
/// Applies environment overrides (`TELEGRAM_API_ID`/`TELEGRAM_API_HASH`,
/// `TELEGRAM_TUI_TELEMETRY`, `DO_NOT_TRACK`) after the file (or defaults)
/// are read, so they always win.
pub fn load() -> eyre::Result<Config> {
    let path = config_path()?;

    let mut cfg = if path.exists() {
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        parse(&text).with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        let defaults = Config::default();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        std::fs::write(&path, defaults.render())
            .with_context(|| format!("failed to write default config to {}", path.display()))?;
        defaults
    };

    apply_env_overrides(&mut cfg);
    Ok(cfg)
}

const KNOWN_SECTIONS: &[&str] = &["app", "keys", "telemetry", "credentials", "consent"];

fn known_keys(section: &str) -> &'static [&'static str] {
    match section {
        "app" => &["theme", "layout_breakpoint_cols", "mouse", "inline_images"],
        "keys" => &["palette"],
        "telemetry" => &["mode", "endpoint", "protocol", "headers"],
        "credentials" => &["api_id", "api_hash"],
        "consent" => &["acknowledged"],
        _ => &[],
    }
}

/// Warns (local log only, per spec §12) on any top-level or in-section key
/// this binary doesn't recognize. `[telemetry.headers]` is exempt: it's a
/// free-form map of header names.
fn warn_unknown_keys(root: &toml::Table) {
    for (key, value) in root {
        if !KNOWN_SECTIONS.contains(&key.as_str()) {
            tracing::warn!(key = %key, "unknown top-level key in config.toml; ignoring");
            continue;
        }
        if key == "telemetry" {
            // headers is a free-form table; only walk the fixed keys.
            if let Some(table) = value.as_table() {
                let known = known_keys(key);
                for sub_key in table.keys() {
                    if sub_key == "headers" {
                        continue;
                    }
                    if !known.contains(&sub_key.as_str()) {
                        tracing::warn!(section = %key, key = %sub_key, "unknown key in config.toml; ignoring");
                    }
                }
            }
            continue;
        }
        if let Some(table) = value.as_table() {
            let known = known_keys(key);
            for sub_key in table.keys() {
                if !known.contains(&sub_key.as_str()) {
                    tracing::warn!(section = %key, key = %sub_key, "unknown key in config.toml; ignoring");
                }
            }
        }
    }
}

/// Parses `text` into a `Config`, warning on unknown keys and erroring
/// (with field-level context) on malformed values.
fn parse(text: &str) -> eyre::Result<Config> {
    let root: toml::Table = toml::from_str(text).context("not valid TOML")?;
    warn_unknown_keys(&root);

    let mut cfg = Config::default();

    if let Some(app) = root.get("app").and_then(toml::Value::as_table) {
        if let Some(v) = app.get("theme") {
            cfg.theme = v
                .as_str()
                .ok_or_else(|| eyre::eyre!("[app].theme must be a string"))?
                .to_string();
        }
        if let Some(v) = app.get("layout_breakpoint_cols") {
            let n = v
                .as_integer()
                .ok_or_else(|| eyre::eyre!("[app].layout_breakpoint_cols must be an integer"))?;
            cfg.layout_breakpoint_cols = u16::try_from(n)
                .map_err(|_| eyre::eyre!("[app].layout_breakpoint_cols out of range for u16"))?;
        }
        if let Some(v) = app.get("mouse") {
            cfg.mouse = v
                .as_bool()
                .ok_or_else(|| eyre::eyre!("[app].mouse must be a boolean"))?;
        }
        if let Some(v) = app.get("inline_images") {
            cfg.inline_images = v
                .as_bool()
                .ok_or_else(|| eyre::eyre!("[app].inline_images must be a boolean"))?;
        }
    }

    if let Some(keys) = root.get("keys").and_then(toml::Value::as_table)
        && let Some(v) = keys.get("palette")
    {
        cfg.palette_key = v
            .as_str()
            .ok_or_else(|| eyre::eyre!("[keys].palette must be a string"))?
            .to_string();
    }

    if let Some(tel) = root.get("telemetry").and_then(toml::Value::as_table) {
        if let Some(v) = tel.get("mode") {
            let s = v
                .as_str()
                .ok_or_else(|| eyre::eyre!("[telemetry].mode must be a string"))?;
            cfg.telemetry_mode = parse_telemetry_mode(s).unwrap_or_else(|| {
                tracing::warn!(mode = %s, "unrecognized [telemetry].mode; defaulting to vendor");
                TelemetryMode::Vendor
            });
        }
        if let Some(v) = tel.get("endpoint") {
            cfg.telemetry_endpoint = Some(
                v.as_str()
                    .ok_or_else(|| eyre::eyre!("[telemetry].endpoint must be a string"))?
                    .to_string(),
            );
        }
        if let Some(v) = tel.get("protocol") {
            cfg.telemetry_protocol = Some(
                v.as_str()
                    .ok_or_else(|| eyre::eyre!("[telemetry].protocol must be a string"))?
                    .to_string(),
            );
        }
        if let Some(headers) = tel.get("headers").and_then(toml::Value::as_table) {
            let mut parsed = Vec::with_capacity(headers.len());
            for (k, v) in headers {
                let value = v
                    .as_str()
                    .ok_or_else(|| eyre::eyre!("[telemetry.headers].{k} must be a string"))?;
                parsed.push((k.clone(), value.to_string()));
            }
            cfg.telemetry_headers = parsed;
        }
    }

    if let Some(creds) = root.get("credentials").and_then(toml::Value::as_table) {
        if let Some(v) = creds.get("api_id") {
            let n = v
                .as_integer()
                .ok_or_else(|| eyre::eyre!("[credentials].api_id must be an integer"))?;
            cfg.api_id = Some(
                i32::try_from(n)
                    .map_err(|_| eyre::eyre!("[credentials].api_id out of range for i32"))?,
            );
        }
        if let Some(v) = creds.get("api_hash") {
            cfg.api_hash = Some(
                v.as_str()
                    .ok_or_else(|| eyre::eyre!("[credentials].api_hash must be a string"))?
                    .to_string(),
            );
        }
    }

    if let Some(consent) = root.get("consent").and_then(toml::Value::as_table)
        && let Some(v) = consent.get("acknowledged")
    {
        cfg.consent_acknowledged = v
            .as_bool()
            .ok_or_else(|| eyre::eyre!("[consent].acknowledged must be a boolean"))?;
    }

    Ok(cfg)
}

/// `TELEGRAM_API_ID`/`TELEGRAM_API_HASH` override file credentials;
/// `TELEGRAM_TUI_TELEMETRY` (`vendor`|`custom`|`off`) overrides the
/// telemetry mode; `DO_NOT_TRACK` set to anything other than empty or `"0"`
/// forces telemetry off regardless of everything else.
fn apply_env_overrides(cfg: &mut Config) {
    if let Ok(raw) = std::env::var("TELEGRAM_API_ID") {
        match raw.parse::<i32>() {
            Ok(id) => cfg.api_id = Some(id),
            Err(_) => {
                tracing::warn!(value = %raw, "TELEGRAM_API_ID is not a valid integer; ignoring")
            }
        }
    }
    if let Ok(hash) = std::env::var("TELEGRAM_API_HASH") {
        cfg.api_hash = Some(hash);
    }

    if let Ok(raw) = std::env::var("TELEGRAM_TUI_TELEMETRY") {
        match parse_telemetry_mode(&raw) {
            Some(mode) => cfg.telemetry_mode = mode,
            None => {
                tracing::warn!(value = %raw, "TELEGRAM_TUI_TELEMETRY is not vendor|custom|off; ignoring")
            }
        }
    }

    if let Ok(raw) = std::env::var("DO_NOT_TRACK")
        && !raw.is_empty()
        && raw != "0"
    {
        cfg.telemetry_mode = TelemetryMode::Off;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    // Config loading touches several process-wide env vars (XDG_CONFIG_HOME
    // plus the override vars). Serialize every test that mutates any of
    // them so parallel `cargo test` runs don't race each other; tolerate
    // poisoning from a prior panicking test rather than cascading failures.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    const RELATED_VARS: &[&str] = &[
        "XDG_CONFIG_HOME",
        "TELEGRAM_API_ID",
        "TELEGRAM_API_HASH",
        "TELEGRAM_TUI_TELEMETRY",
        "DO_NOT_TRACK",
    ];

    /// Clears every env var `load()` reads so ambient state on the test
    /// runner's machine can't leak into a test's expectations.
    fn clear_related_env() {
        // SAFETY: caller holds `ENV_LOCK`, so no other thread in this test
        // binary reads or writes these vars concurrently.
        unsafe {
            for var in RELATED_VARS {
                std::env::remove_var(var);
            }
        }
    }

    #[test]
    fn generates_commented_default_on_first_run() {
        let _lock = lock_env();
        clear_related_env();
        let tmp = tempfile::tempdir().unwrap();
        // SAFETY: serialized by ENV_LOCK.
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let cfg = load().expect("load should generate and return defaults");

        let path = tmp.path().join("telegram-tui").join("config.toml");
        let text =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("expected {path:?}: {e}"));
        assert!(
            text.lines().any(|l| l.trim_start().starts_with('#')),
            "expected comment lines in generated config, got:\n{text}"
        );
        assert_eq!(cfg.theme, "default");
        assert_eq!(cfg.layout_breakpoint_cols, 100);
        assert_eq!(cfg.palette_key, "ctrl+p");
        assert_eq!(cfg.telemetry_mode, TelemetryMode::Vendor);
        assert_eq!(cfg.api_id, None);
        assert!(!cfg.consent_acknowledged);

        clear_related_env();
    }

    /// Mouse support is on unless the config says otherwise, and a config
    /// written before the key existed keeps that default — the generated
    /// file has to survive the round trip either way (architecture §7.5).
    #[test]
    fn mouse_defaults_on_and_can_be_turned_off() {
        let _lock = lock_env();
        clear_related_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let cfg = load().expect("first run generates defaults");
        assert!(cfg.mouse, "mouse reporting is on by default");
        let generated = std::fs::read_to_string(tmp.path().join("telegram-tui/config.toml"))
            .expect("the generated config should be readable");
        assert!(
            generated.contains("mouse = true"),
            "the generated template should document the key:\n{generated}"
        );

        std::fs::write(
            tmp.path().join("telegram-tui/config.toml"),
            "[app]\nmouse = false\n",
        )
        .unwrap();
        assert!(!load().expect("load should succeed").mouse);

        // A config from before the key existed still boots with it on.
        std::fs::write(
            tmp.path().join("telegram-tui/config.toml"),
            "[app]\ntheme = \"midnight\"\n",
        )
        .unwrap();
        assert!(load().expect("load should succeed").mouse);

        clear_related_env();
    }

    /// Inline images are on unless the config says otherwise — the terminal
    /// probe decides whether anything comes of that (design-language §6) —
    /// and, like `mouse`, a config written before the key existed keeps the
    /// default.
    ///
    /// Drives `render`/`parse` rather than `load`, so it needs neither the
    /// filesystem nor `XDG_CONFIG_HOME`: `load`'s own first-run-generates
    /// and file-round-trip behavior is already covered by the tests above,
    /// and every test that sets that variable is one more thread racing the
    /// tests in `otel.rs` that set it under a *different* lock.
    #[test]
    fn inline_images_default_on_and_can_be_turned_off() {
        let defaults = Config::default();
        assert!(defaults.inline_images, "inline images are on by default");
        let generated = defaults.render();
        assert!(
            generated.contains("inline_images = true"),
            "the generated template should document the key:\n{generated}"
        );
        assert!(
            parse(&generated)
                .expect("the generated template must parse")
                .inline_images,
            "the template has to round-trip through the parser"
        );

        assert!(
            !parse("[app]\ninline_images = false\n")
                .expect("parse should succeed")
                .inline_images
        );
        // A config from before the key existed still boots with it on.
        assert!(
            parse("[app]\ntheme = \"midnight\"\n")
                .expect("parse should succeed")
                .inline_images
        );
        // And a non-boolean is a loud error, like every other typed key.
        assert!(parse("[app]\ninline_images = \"yes\"\n").is_err());
    }

    #[test]
    fn unknown_keys_warn_but_load() {
        let _lock = lock_env();
        clear_related_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let dir = tmp.path().join("telegram-tui");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            r#"
                from_the_future = true

                [app]
                theme = "midnight"
                layout_breakpoint_cols = 120
                some_new_field = "ignored"

                [telemetry]
                mode = "off"
            "#,
        )
        .unwrap();

        let cfg = load().expect("unknown keys should warn, not fail");
        assert_eq!(cfg.theme, "midnight");
        assert_eq!(cfg.layout_breakpoint_cols, 120);
        assert_eq!(cfg.telemetry_mode, TelemetryMode::Off);

        clear_related_env();
    }

    #[test]
    fn env_overrides_beat_file() {
        let _lock = lock_env();
        clear_related_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let dir = tmp.path().join("telegram-tui");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            r#"
                [credentials]
                api_id = 111
                api_hash = "file-hash"
            "#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("TELEGRAM_API_ID", "222");
            std::env::set_var("TELEGRAM_API_HASH", "env-hash");
        }

        let cfg = load().expect("load should succeed");
        assert_eq!(cfg.api_id, Some(222));
        assert_eq!(cfg.api_hash.as_deref(), Some("env-hash"));

        clear_related_env();
    }

    #[test]
    fn do_not_track_forces_mode_off() {
        let _lock = lock_env();
        clear_related_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let dir = tmp.path().join("telegram-tui");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            r#"
                [telemetry]
                mode = "vendor"
            "#,
        )
        .unwrap();

        unsafe {
            std::env::set_var("DO_NOT_TRACK", "1");
        }
        let cfg = load().expect("load should succeed");
        assert_eq!(cfg.telemetry_mode, TelemetryMode::Off);

        // "0" is the documented opt-out-of-opt-out and must not force Off.
        unsafe {
            std::env::set_var("DO_NOT_TRACK", "0");
        }
        let cfg = load().expect("load should succeed");
        assert_eq!(cfg.telemetry_mode, TelemetryMode::Vendor);

        clear_related_env();
    }

    #[test]
    fn apply_patch_roundtrips() {
        let _lock = lock_env();
        clear_related_env();
        let tmp = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        }

        let mut cfg = load().expect("initial load should generate defaults");
        cfg.apply_patch(&ConfigPatch::Theme("solarized".to_string()));
        cfg.apply_patch(&ConfigPatch::Credentials {
            api_id: 42,
            api_hash: "s3cr3t".to_string(),
        });
        cfg.apply_patch(&ConfigPatch::ConsentAcknowledged { enabled: true });
        cfg.apply_patch(&ConfigPatch::TelemetryMode(TelemetryMode::Custom));
        cfg.save().expect("save should succeed");

        let reloaded = load().expect("reload should succeed");
        assert_eq!(reloaded.theme, "solarized");
        assert_eq!(reloaded.api_id, Some(42));
        assert_eq!(reloaded.api_hash.as_deref(), Some("s3cr3t"));
        assert!(reloaded.consent_acknowledged);
        assert_eq!(reloaded.telemetry_mode, TelemetryMode::Custom);

        clear_related_env();
    }

    #[test]
    fn parse_key_handles_ctrl_and_char_and_falls_back() {
        assert_eq!(parse_key("ctrl+p", Key::Esc), Key::Ctrl('p'));
        assert_eq!(parse_key("?", Key::Esc), Key::Char('?'));
        assert_eq!(parse_key("not-a-key", Key::Esc), Key::Esc);
    }

    #[test]
    fn custom_mode_endpoint_replaces_vendor() {
        let mut cfg = Config {
            telemetry_mode: TelemetryMode::Custom,
            telemetry_endpoint: Some("https://collector.example/".to_string()),
            telemetry_protocol: Some("http/json".to_string()),
            telemetry_headers: vec![("x-scope-orgid".to_string(), "42".to_string())],
            ..Config::default()
        };

        let dest = cfg
            .custom_destination()
            .expect("custom mode with an endpoint resolves to a destination");
        assert_eq!(dest.endpoint, "https://collector.example/");
        assert_eq!(dest.protocol.as_deref(), Some("http/json"));
        assert_eq!(
            dest.headers,
            vec![("x-scope-orgid".to_string(), "42".to_string())]
        );

        // Vendor mode never resolves a custom destination, even with the
        // same fields populated: custom fully replaces vendor, and the two
        // are never combined (spec §13.5).
        cfg.telemetry_mode = TelemetryMode::Vendor;
        assert!(
            cfg.custom_destination().is_none(),
            "vendor mode must not see a custom destination"
        );

        // Custom mode with no endpoint configured is inert, same as an
        // unset vendor endpoint.
        cfg.telemetry_mode = TelemetryMode::Custom;
        cfg.telemetry_endpoint = None;
        assert!(cfg.custom_destination().is_none());
    }

    #[test]
    fn boot_fields_reports_has_credentials() {
        let mut cfg = Config::default();
        assert!(!cfg.boot_fields().has_credentials);
        cfg.api_id = Some(1);
        cfg.api_hash = Some("h".to_string());
        assert!(cfg.boot_fields().has_credentials);
    }
}
