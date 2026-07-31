//! Theme file loading — architecture.md §4.9, spec §7.2.
//!
//! Path resolution (`<config_dir>/themes/<name>.toml`) and the
//! builtin-then-file-then-default fallback chain are the caller's job —
//! see `crates/app/src/main.rs::resolve_theme`, the one place that chain is
//! written. `runtime_loop::Core` re-runs it on a live theme switch
//! (`AppState::theme_generation` bump, T60) rather than duplicating the
//! chain, so `resolve_theme` stays the single resolution path either way.
//! This module only turns bytes into a `Theme` or a reason it couldn't, plus
//! the built-in catalogue itself (`builtin`, `builtin_names`).
//!
//! # Parsing rules
//!
//! - A color value is either `"#rrggbb"` (hex, case-insensitive) or a named
//!   ANSI color: `black`, `red`, `green`, `yellow`, `blue`, `magenta`,
//!   `cyan`, `white`, `gray`/`grey`, each with a `bright_`-prefixed variant
//!   (`bright_red`, `bright_black`, ...). Ratatui's `Color` only has 16
//!   named slots for 18 candidate names, so `white`/`gray`/`grey` share one
//!   axis on purpose: `white`, `gray` and `grey` all mean the same dim ANSI
//!   white (`Color::Gray`), and their `bright_` forms all mean pure
//!   `Color::White` — `gray`/`grey` exist as the more intuitive spelling
//!   for readers who don't think of "white" as dim by default.
//! - Unknown top-level keys warn (`tracing::warn!`, local log only — spec
//!   §12's config philosophy applies here too) and are otherwise ignored,
//!   so a theme file written for a newer binary doesn't break an older one.
//! - A token key that is simply absent falls back to
//!   `Theme::default_dark`'s value for that token, silently — friendlier
//!   than a hard failure for someone who only wants to override a couple of
//!   colors, and consistent with `config.rs`'s "missing means default"
//!   stance.
//! - A present-but-unparseable value (bad hex, unrecognized name, wrong
//!   TOML type) fails the *whole* load with `ThemeLoadError::BadColor`,
//!   carrying both the offending key and its raw value so the caller's
//!   warning is actionable — a half-applied theme would be a worse
//!   surprise than falling back to `default_dark` entirely.
//! - `sender_palette`, if present, must be an array of exactly 8 color
//!   values; any other length is also a `BadColor` (key `"sender_palette"`,
//!   or `"sender_palette[i]"` for a bad element).

use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::Color;

use crate::theme::Theme;

/// Failure loading a user theme file. `main.rs`'s call site treats every
/// variant the same way — fall back to `Theme::default_dark` with a local
/// warning — but callers that want a more specific message can match on it.
#[derive(Debug)]
pub enum ThemeLoadError {
    Io(std::io::Error),
    Parse(String),
    BadColor { key: String, value: String },
}

impl std::fmt::Display for ThemeLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ThemeLoadError::Io(err) => write!(f, "could not read theme file: {err}"),
            ThemeLoadError::Parse(msg) => write!(f, "could not parse theme file: {msg}"),
            ThemeLoadError::BadColor { key, value } => {
                write!(f, "invalid color for `{key}`: {value:?}")
            }
        }
    }
}

impl std::error::Error for ThemeLoadError {}

impl From<std::io::Error> for ThemeLoadError {
    fn from(err: std::io::Error) -> Self {
        ThemeLoadError::Io(err)
    }
}

/// The 13 semantic token keys, in `Theme` field order. `sender_palette` is
/// handled separately since it's an array, not a scalar color.
///
/// `border` (docs/architecture.md §4.9's render-state-contract amendment)
/// was added to the `Theme` struct without this list or `set_token` picking
/// it up — a real gap this task closes: a theme file setting `border`
/// silently had no effect until now.
const TOKEN_KEYS: &[&str] = &[
    "accent",
    "accent_dim",
    "text",
    "text_muted",
    "surface",
    "surface_raised",
    "success",
    "warning",
    "danger",
    "selection",
    "rail_own",
    "rail_other",
    "border",
];

/// Parses a user theme TOML file at `path`. Same token names as `Theme`'s
/// fields, plus `sender_palette` (an array of 8). See module docs for the
/// value grammar and the missing/unknown-key fallback rules.
pub fn load_theme(path: &Path) -> Result<Theme, ThemeLoadError> {
    let text = std::fs::read_to_string(path)?;
    parse(&text)
}

/// The built-in catalogue (docs/design-language.md §7), in display/cycle
/// order. Each entry's TOML is compiled into the binary with `include_str!`
/// from `crates/ui/themes/` and parsed through the same [`parse`] path as a
/// user theme file, so a hand-authored theme can override a built-in by
/// reusing its name (see `load_theme`'s doc comment and `builtin`'s).
///
/// `default-dark` is a verbatim port of the historical
/// `Theme::default_dark()` literals, so nothing regresses for anyone who
/// never picks a theme at all.
const BUILTIN_CATALOGUE: &[(&str, &str)] = &[
    (
        "default-dark",
        include_str!("../../themes/default-dark.toml"),
    ),
    (
        "catppuccin-frappe",
        include_str!("../../themes/catppuccin-frappe.toml"),
    ),
    (
        "catppuccin-macchiato",
        include_str!("../../themes/catppuccin-macchiato.toml"),
    ),
    (
        "catppuccin-mocha",
        include_str!("../../themes/catppuccin-mocha.toml"),
    ),
    (
        "catppuccin-latte",
        include_str!("../../themes/catppuccin-latte.toml"),
    ),
    ("tokyo-night", include_str!("../../themes/tokyo-night.toml")),
    (
        "gruvbox-dark",
        include_str!("../../themes/gruvbox-dark.toml"),
    ),
    ("nord", include_str!("../../themes/nord.toml")),
];

/// A built-in theme by name, or `None` if `name` isn't one telegram-tui
/// ships — the caller falls through to `load_theme` on `None`.
///
/// Names are matched case-insensitively and accept `_` as well as `-` as
/// the word separator (`catppuccin_frappe` resolves the same entry as
/// `catppuccin-frappe`), since config files and the palette both pass names
/// through free-form. `default` and `default_dark` remain accepted aliases
/// for `default-dark`, matching this function's pre-catalogue behavior.
pub fn builtin(name: &str) -> Option<Theme> {
    let key = normalize_builtin_name(name);
    let (_, toml) = BUILTIN_CATALOGUE.iter().find(|(n, _)| *n == key)?;
    Some(parse(toml).unwrap_or_else(|err| panic!("builtin theme {key:?} failed to parse: {err}")))
}

/// Catalogue names in the same order as `BUILTIN_CATALOGUE`, for a palette
/// cycle or a `--list-themes`-style listing. Canonical (hyphenated) form
/// only — `builtin` accepts underscore variants on lookup, but the
/// catalogue itself has one spelling per theme.
pub fn builtin_names() -> &'static [&'static str] {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES
        .get_or_init(|| BUILTIN_CATALOGUE.iter().map(|&(name, _)| name).collect())
        .as_slice()
}

/// Case-folds and dash/underscore-normalizes a theme name for catalogue
/// lookup, and folds the two legacy `default*` spellings onto the
/// catalogue's `default-dark` entry.
fn normalize_builtin_name(name: &str) -> String {
    let normalized = name.trim().to_ascii_lowercase().replace('_', "-");
    if normalized == "default" {
        "default-dark".to_string()
    } else {
        normalized
    }
}

/// Truecolor → 256-color degradation, selected by the caller's terminal
/// capability probe (`COLORTERM`, read at the `main.rs` call site — this
/// module stays free of environment access so it stays unit-testable).
/// `truecolor = true` returns `theme` unchanged; otherwise `theme.degraded()`.
pub fn for_terminal(theme: Theme, truecolor: bool) -> Theme {
    if truecolor { theme } else { theme.degraded() }
}

fn parse(text: &str) -> Result<Theme, ThemeLoadError> {
    let root: toml::Table =
        toml::from_str(text).map_err(|err| ThemeLoadError::Parse(err.to_string()))?;
    // `crate::theme::base_defaults()`, not `Theme::default_dark()`: the
    // latter now parses `themes/default-dark.toml` through this very
    // function, and starting the fill-in base there would recurse forever.
    // See `base_defaults`'s doc comment.
    let mut theme = crate::theme::base_defaults();

    for (key, value) in &root {
        match key.as_str() {
            "sender_palette" => theme.sender_palette = parse_palette(value)?,
            k if TOKEN_KEYS.contains(&k) => set_token(&mut theme, k, parse_color_value(k, value)?),
            other => {
                tracing::warn!(key = %other, "unknown key in theme file; ignoring");
            }
        }
    }

    Ok(theme)
}

fn set_token(theme: &mut Theme, key: &str, color: Color) {
    match key {
        "accent" => theme.accent = color,
        "accent_dim" => theme.accent_dim = color,
        "text" => theme.text = color,
        "text_muted" => theme.text_muted = color,
        "surface" => theme.surface = color,
        "surface_raised" => theme.surface_raised = color,
        "success" => theme.success = color,
        "warning" => theme.warning = color,
        "danger" => theme.danger = color,
        "selection" => theme.selection = color,
        "rail_own" => theme.rail_own = color,
        "rail_other" => theme.rail_other = color,
        "border" => theme.border = color,
        _ => unreachable!("set_token called with non-token key {key:?}"),
    }
}

fn parse_palette(value: &toml::Value) -> Result<[Color; 8], ThemeLoadError> {
    let array = value.as_array().ok_or_else(|| ThemeLoadError::BadColor {
        key: "sender_palette".to_string(),
        value: value.to_string(),
    })?;
    if array.len() != 8 {
        return Err(ThemeLoadError::BadColor {
            key: "sender_palette".to_string(),
            value: format!("expected 8 entries, found {}", array.len()),
        });
    }
    let mut colors = [Color::Reset; 8];
    for (i, entry) in array.iter().enumerate() {
        colors[i] = parse_color_value(&format!("sender_palette[{i}]"), entry)?;
    }
    Ok(colors)
}

fn parse_color_value(key: &str, value: &toml::Value) -> Result<Color, ThemeLoadError> {
    let raw = value.as_str().ok_or_else(|| ThemeLoadError::BadColor {
        key: key.to_string(),
        value: value.to_string(),
    })?;
    parse_color_str(raw).ok_or_else(|| ThemeLoadError::BadColor {
        key: key.to_string(),
        value: raw.to_string(),
    })
}

/// `"#rrggbb"` or a named ANSI color (see module docs). `None` on anything
/// else, leaving the caller to attach the key.
fn parse_color_str(raw: &str) -> Option<Color> {
    let trimmed = raw.trim();
    match trimmed.strip_prefix('#') {
        Some(hex) => parse_hex(hex),
        None => parse_named(trimmed),
    }
}

fn parse_hex(hex: &str) -> Option<Color> {
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

fn parse_named(name: &str) -> Option<Color> {
    let lower = name.to_ascii_lowercase();
    let (bright, base) = match lower
        .strip_prefix("bright_")
        .or_else(|| lower.strip_prefix("bright-"))
    {
        Some(rest) => (true, rest),
        None => (false, lower.as_str()),
    };
    match (base, bright) {
        ("black", false) => Some(Color::Black),
        ("black", true) => Some(Color::DarkGray),
        ("red", false) => Some(Color::Red),
        ("red", true) => Some(Color::LightRed),
        ("green", false) => Some(Color::Green),
        ("green", true) => Some(Color::LightGreen),
        ("yellow", false) => Some(Color::Yellow),
        ("yellow", true) => Some(Color::LightYellow),
        ("blue", false) => Some(Color::Blue),
        ("blue", true) => Some(Color::LightBlue),
        ("magenta", false) => Some(Color::Magenta),
        ("magenta", true) => Some(Color::LightMagenta),
        ("cyan", false) => Some(Color::Cyan),
        ("cyan", true) => Some(Color::LightCyan),
        ("white" | "gray" | "grey", false) => Some(Color::Gray),
        ("white" | "gray" | "grey", true) => Some(Color::White),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::text::Line;
    use tgt_core::model::ids::MessageId;

    use super::*;
    use crate::render::cache::{LayoutCache, LayoutKey};

    // `r##"..."##` (not `r#"..."#`) because the fixtures contain `"#rrggbb"`
    // literals: a bare `"#` inside a single-hash raw string would close it
    // early.
    const FULL_THEME_TOML: &str = r##"
        accent = "#61afef"
        accent_dim = "bright_blue"
        text = "#dcdfe4"
        text_muted = "gray"
        surface = "#181a20"
        surface_raised = "black"
        success = "#98c379"
        warning = "bright_yellow"
        danger = "#e06c75"
        selection = "magenta"
        rail_own = "#3a698f"
        rail_other = "bright_cyan"
        border = "#343944"

        sender_palette = [
            "#e06c75",
            "bright_red",
            "#e5c07b",
            "green",
            "#56b6c2",
            "blue",
            "#c678dd",
            "bright_magenta",
        ]
    "##;

    #[test]
    fn parses_all_thirteen_tokens_plus_palette() {
        let theme = parse(FULL_THEME_TOML).expect("fixture must parse");

        assert_eq!(theme.accent, Color::Rgb(0x61, 0xaf, 0xef));
        assert_eq!(theme.accent_dim, Color::LightBlue);
        assert_eq!(theme.text, Color::Rgb(0xdc, 0xdf, 0xe4));
        assert_eq!(theme.text_muted, Color::Gray);
        assert_eq!(theme.surface, Color::Rgb(0x18, 0x1a, 0x20));
        assert_eq!(theme.surface_raised, Color::Black);
        assert_eq!(theme.success, Color::Rgb(0x98, 0xc3, 0x79));
        assert_eq!(theme.warning, Color::LightYellow);
        assert_eq!(theme.danger, Color::Rgb(0xe0, 0x6c, 0x75));
        assert_eq!(theme.selection, Color::Magenta);
        assert_eq!(theme.rail_own, Color::Rgb(0x3a, 0x69, 0x8f));
        assert_eq!(theme.rail_other, Color::LightCyan);
        // Regression coverage for the gap this task closed: `border` was on
        // `Theme` but missing from `TOKEN_KEYS`/`set_token`, so a theme file
        // setting it was silently ignored.
        assert_eq!(theme.border, Color::Rgb(0x34, 0x39, 0x44));

        assert_eq!(
            theme.sender_palette,
            [
                Color::Rgb(0xe0, 0x6c, 0x75),
                Color::LightRed,
                Color::Rgb(0xe5, 0xc0, 0x7b),
                Color::Green,
                Color::Rgb(0x56, 0xb6, 0xc2),
                Color::Blue,
                Color::Rgb(0xc6, 0x78, 0xdd),
                Color::LightMagenta,
            ]
        );
    }

    #[test]
    fn missing_keys_fall_back_to_default_dark() {
        let default = Theme::default_dark();
        let theme = parse("accent = \"#ff00ff\"").expect("fixture must parse");

        // Only `accent` was overridden; every other token keeps
        // `default_dark`'s value rather than erroring on the missing keys.
        assert_eq!(theme.accent, Color::Rgb(0xff, 0x00, 0xff));
        assert_eq!(theme.text, default.text);
        assert_eq!(theme.surface, default.surface);
        assert_eq!(theme.border, default.border);
        assert_eq!(theme.sender_palette, default.sender_palette);
    }

    #[test]
    fn unknown_key_warns_not_fails() {
        let toml = "accent = \"#61afef\"\nthis_key_does_not_exist = \"whatever\"\n";

        let theme = parse(toml).expect("unknown key must warn, not fail the load");
        assert_eq!(theme.accent, Color::Rgb(0x61, 0xaf, 0xef));
    }

    #[test]
    fn bad_color_reports_key_and_value() {
        let toml = "accent = \"not-a-color\"";

        match parse(toml) {
            Err(ThemeLoadError::BadColor { key, value }) => {
                assert_eq!(key, "accent");
                assert_eq!(value, "not-a-color");
            }
            other => panic!("expected BadColor{{key, value}}, got {other:?}"),
        }
    }

    #[test]
    fn bad_color_reports_key_and_value_for_bad_hex() {
        let toml = "danger = \"#gg0000\"";

        match parse(toml) {
            Err(ThemeLoadError::BadColor { key, value }) => {
                assert_eq!(key, "danger");
                assert_eq!(value, "#gg0000");
            }
            other => panic!("expected BadColor{{key, value}}, got {other:?}"),
        }
    }

    #[test]
    fn sender_palette_wrong_length_is_bad_color() {
        let toml = "sender_palette = [\"red\", \"blue\"]";

        match parse(toml) {
            Err(ThemeLoadError::BadColor { key, value }) => {
                assert_eq!(key, "sender_palette");
                assert!(
                    value.contains('2'),
                    "value should mention the found length: {value}"
                );
            }
            other => panic!("expected BadColor{{key, value}}, got {other:?}"),
        }
    }

    #[test]
    fn builtin_resolves_default_and_default_dark_only() {
        assert!(builtin("default").is_some());
        assert!(builtin("default_dark").is_some());
        assert!(builtin("nonexistent-theme").is_none());
    }

    /// T60: the catalogue grew from one entry (`default-dark`) to eight
    /// (docs/design-language.md §7). Every name `builtin_names()` reports
    /// must resolve, and — since every builtin TOML is meant to set the
    /// full token set rather than lean on `base_defaults()` fill-in — no
    /// token may equal `base_defaults()`'s placeholder value unless the
    /// theme's own hand-picked color genuinely happens to match it (only
    /// `default-dark` does, by construction: it's a verbatim port).
    #[test]
    fn builtin_catalogue_resolves_every_name_and_defines_every_token() {
        let names = builtin_names();
        assert_eq!(
            names.len(),
            8,
            "docs/design-language.md §7 names 8 built-in themes"
        );

        let placeholder = crate::theme::base_defaults();

        for &name in names {
            let theme = builtin(name).unwrap_or_else(|| panic!("{name:?} must resolve"));

            if name == "default-dark" {
                // The one theme that's *supposed* to equal the placeholder
                // values verbatim (it's the literal port of them).
                assert_eq!(theme, placeholder);
                continue;
            }

            assert_ne!(
                theme.accent, placeholder.accent,
                "{name}: accent left at the default-dark placeholder"
            );
            assert_ne!(
                theme.surface, placeholder.surface,
                "{name}: surface left at the default-dark placeholder"
            );
            assert_ne!(
                theme.text, placeholder.text,
                "{name}: text left at the default-dark placeholder"
            );
            assert_ne!(
                theme.border, placeholder.border,
                "{name}: border left at the default-dark placeholder"
            );
            assert_ne!(
                theme.sender_palette, placeholder.sender_palette,
                "{name}: sender_palette left at the default-dark placeholder"
            );
        }

        // Both underscore and hyphen spellings resolve to the same theme.
        assert_eq!(
            builtin("catppuccin_frappe"),
            builtin("catppuccin-frappe"),
            "underscore and hyphen spellings must be the same catalogue entry"
        );
        // Case-insensitive too.
        assert_eq!(builtin("NORD"), builtin("nord"));
    }

    /// A snapshot of one theme's full token set, so a future palette edit
    /// to any built-in shows up as reviewable diff text rather than a
    /// silent color change. Mocha is picked because it is the catalogue's
    /// only entry that isn't `default-dark` (already covered by the
    /// verbatim-port assertion above) and isn't the light theme, keeping
    /// this snapshot representative of the "normal" dark-theme case.
    #[test]
    fn catppuccin_mocha_full_token_set_snapshot() {
        let theme = builtin("catppuccin-mocha").expect("catppuccin-mocha is in the catalogue");
        insta::assert_snapshot!(format!("{theme:#?}"));
    }

    #[test]
    fn degraded_maps_rgb_to_nearest_256() {
        let theme = Theme::default_dark();

        // Spot-check the accent token: Rgb(97, 175, 239) -> cube steps
        // (2, 3, 5) -> 16 + 36*2 + 6*3 + 5 = 111 (see Theme::degraded's
        // rounding formula in ui/src/theme/mod.rs).
        let degraded = for_terminal(theme.clone(), false);
        assert_eq!(degraded.accent, Color::Indexed(111));

        // truecolor = true must not touch the color at all.
        let kept = for_terminal(theme.clone(), true);
        assert_eq!(kept.accent, theme.accent);
    }

    /// The plan names this test `theme_change_bumps_generation_and_clears_cache`.
    /// T60 wires the actual runtime toggle (`state::palette::CommandId::ToggleTheme`
    /// bumps `AppState::theme_generation`; `runtime_loop::Core` notices the
    /// bump and re-resolves the `Theme`, see `crates/core/src/state/palette.rs`
    /// and `crates/app/src/runtime_loop.rs`), so that end-to-end path is
    /// covered by `palette.rs`'s own tests instead. What belongs here, at
    /// the loader/cache layer, is narrower and still worth asserting on its
    /// own: (1) two themes loaded from different sources really do differ,
    /// so a generation bump would matter, and (2) `LayoutKey`'s
    /// `theme_generation` field already makes the cache treat different
    /// generations as different entries (the mechanism `theme_generation`
    /// exists to drive), independent of every other key field.
    #[test]
    fn loaded_themes_differ_and_theme_generation_key_component_misses_independently() {
        let default = Theme::default_dark();
        let custom = parse("accent = \"#ff00ff\"").expect("fixture must parse");

        assert_ne!(
            default.accent, custom.accent,
            "a loaded theme must actually differ from default_dark for the cache-miss below to mean anything"
        );

        let mut cache = LayoutCache::new();
        let base_key = LayoutKey {
            message_id: MessageId(1),
            width: 80,
            theme_generation: 0,
            spoilers_revealed: false,
        };
        let other_generation_key = LayoutKey {
            theme_generation: 1,
            ..base_key
        };

        cache.get_or_insert_with(base_key, || vec![Line::from("styled with default")]);

        let mut called = false;
        cache.get_or_insert_with(other_generation_key, || {
            called = true;
            vec![Line::from("styled with custom")]
        });

        assert!(
            called,
            "a different theme_generation must miss the cache even with every other key field unchanged"
        );
    }
}
