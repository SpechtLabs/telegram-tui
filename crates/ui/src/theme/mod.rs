//! Semantic color tokens. Colors are never written at call sites: every view
//! reads a token off `Theme`. See docs/architecture.md §4.9, spec §7.2.

pub mod loader;

use ratatui::style::Color;

#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub accent: Color,
    pub accent_dim: Color,
    pub text: Color,
    pub text_muted: Color,
    pub surface: Color,
    pub surface_raised: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub selection: Color,
    pub rail_own: Color,
    pub rail_other: Color,
    /// Curated palette for deterministic per-sender accents.
    pub sender_palette: [Color; 8],
}

impl Theme {
    /// The built-in dark theme: a desaturated slate surface with a blue accent.
    pub fn default_dark() -> Theme {
        Theme {
            accent: Color::Rgb(97, 175, 239),
            accent_dim: Color::Rgb(58, 105, 143),
            text: Color::Rgb(220, 223, 228),
            text_muted: Color::Rgb(140, 146, 156),
            surface: Color::Rgb(24, 26, 32),
            surface_raised: Color::Rgb(34, 37, 45),
            success: Color::Rgb(152, 195, 121),
            warning: Color::Rgb(229, 192, 123),
            danger: Color::Rgb(224, 108, 117),
            selection: Color::Rgb(49, 59, 79),
            rail_own: Color::Rgb(58, 105, 143),
            rail_other: Color::Rgb(97, 175, 239),
            sender_palette: [
                Color::Rgb(224, 108, 117), // red
                Color::Rgb(209, 154, 102), // orange
                Color::Rgb(229, 192, 123), // yellow
                Color::Rgb(152, 195, 121), // green
                Color::Rgb(86, 182, 194),  // cyan
                Color::Rgb(97, 175, 239),  // blue
                Color::Rgb(198, 120, 221), // purple
                Color::Rgb(224, 130, 170), // pink
            ],
        }
    }

    /// Same sender → same color across sessions: seed % palette length.
    pub fn sender_color(&self, color_seed: i64) -> Color {
        self.sender_palette[(color_seed.unsigned_abs() % 8) as usize]
    }

    /// Truecolor → 256-color degradation for terminals without RGB.
    pub fn degraded(&self) -> Theme {
        Theme {
            accent: degrade(self.accent),
            accent_dim: degrade(self.accent_dim),
            text: degrade(self.text),
            text_muted: degrade(self.text_muted),
            surface: degrade(self.surface),
            surface_raised: degrade(self.surface_raised),
            success: degrade(self.success),
            warning: degrade(self.warning),
            danger: degrade(self.danger),
            selection: degrade(self.selection),
            rail_own: degrade(self.rail_own),
            rail_other: degrade(self.rail_other),
            sender_palette: self.sender_palette.map(degrade),
        }
    }
}

/// Map a truecolor `Color::Rgb` to the nearest color in the 256-color cube
/// (indices 16..=231, a 6x6x6 cube of steps `[0, 95, 135, 175, 215, 255]`).
/// Non-Rgb colors pass through unchanged.
fn degrade(color: Color) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    // Round each channel to the nearest of the 6 cube steps by mapping
    // 0..=255 onto 0..=5 with rounding.
    let to_cube = |channel: u8| -> u16 { ((channel as u16) * 5 + 127) / 255 };
    let (r6, g6, b6) = (to_cube(r), to_cube(g), to_cube(b));
    let index = 16 + 36 * r6 + 6 * g6 + b6;
    Color::Indexed(index as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sender_color_deterministic() {
        let theme = Theme::default_dark();

        // Same seed always yields the same color.
        for seed in [-17i64, -8, -1, 0, 1, 7, 8, 100, i64::MIN, i64::MAX] {
            assert_eq!(theme.sender_color(seed), theme.sender_color(seed));
        }

        // seed % 8 distribution, including negative seeds via unsigned_abs.
        for i in 0..8i64 {
            assert_eq!(theme.sender_color(i), theme.sender_palette[i as usize]);
            assert_eq!(theme.sender_color(-i), theme.sender_palette[i as usize]);
            assert_eq!(theme.sender_color(i + 8), theme.sender_palette[i as usize]);
        }

        // i64::MIN has no positive counterpart; unsigned_abs must not panic.
        let _ = theme.sender_color(i64::MIN);
    }

    #[test]
    fn degraded_maps_rgb_to_indexed() {
        let theme = Theme::default_dark();
        let degraded = theme.degraded();
        assert!(matches!(degraded.accent, Color::Indexed(_)));
        assert!(matches!(degraded.sender_palette[0], Color::Indexed(_)));
    }
}
