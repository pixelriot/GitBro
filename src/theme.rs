use std::{collections::HashMap, path::PathBuf};

use rune::color::srgb_decode;
use rune::{Color, Spring};
use rune_kit::{Kit, PaletteFocus, PaletteSeed, Radius, StateColors};

/// Colors as 0xRRGGBB, derived from the active Omarchy theme's `colors.toml`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Theme {
    pub bg: u32,
    pub surface: u32,
    pub border: u32,
    pub selected: u32,
    pub muted: u32,
    pub text: u32,
    pub subtext: u32,
    pub accent: u32,
    pub green: u32,
    pub red: u32,
    pub yellow: u32,
    pub lanes: [u32; crate::graph::PALETTE_SIZE],
}

impl Default for Theme {
    /// Catppuccin Mocha, used when no Omarchy theme can be read.
    fn default() -> Self {
        Self {
            bg: 0x1e1e2e,
            surface: 0x181825,
            border: 0x313244,
            selected: 0x45475a,
            muted: 0x585b70,
            text: 0xcdd6f4,
            subtext: 0x9399b2,
            accent: 0x89b4fa,
            green: 0xa6e3a1,
            red: 0xf38ba8,
            yellow: 0xf9e2af,
            lanes: [
                0x89b4fa, 0xa6e3a1, 0xf9e2af, 0xf38ba8, 0xcba6f7, 0x94e2d5, 0xfab387, 0xf5c2e7,
            ],
        }
    }
}

impl Theme {
    /// The theme Omarchy currently has applied, or the default if it can't be read.
    pub fn current() -> Self {
        Self::read().unwrap_or_default()
    }

    /// One of the theme's `0xRRGGBB` colors as a Rune linear-light color.
    pub fn c(&self, color: u32) -> Color {
        let channel = |shift: u32| srgb_decode(((color >> shift) & 0xff) as f32 / 255.0);
        Color {
            r: channel(16),
            g: channel(8),
            b: channel(0),
            a: 1.0,
        }
    }

    pub fn lane(&self, i: usize) -> Color {
        self.c(self.lanes[i % self.lanes.len()])
    }

    /// A Rune-kit theme matching the Omarchy palette, with angular corners and quick springs.
    pub fn kit(&self) -> Kit {
        let bg = self.c(self.bg);
        let surface = self.c(self.surface);
        let border = self.c(self.border);
        let selected = self.c(self.selected);
        let text = self.c(self.text);
        let subtext = self.c(self.subtext);
        let accent = self.c(self.accent);
        let mut k = Kit::from_palette_focus(
            PaletteSeed {
                bg,
                fg: text,
                accent,
            },
            PaletteFocus::Accent,
        );
        // GitBro draws crisp rectangles, not pill-shaped controls.
        k.radius = Radius {
            xs: 0.0,
            sm: 1.0,
            md: 2.0,
            lg: 2.0,
            xl: 3.0,
        };
        k.motion.control = Spring::new(0.24, 0.0);
        k.motion.overlay = Spring::new(0.3, 0.12);

        k.c.page = bg;
        k.c.bg_base = bg;
        k.c.bg_deep = bg;
        k.c.layer_01 = surface;
        k.c.layer_02 = border;
        k.c.layer_03 = selected;
        k.c.layer_04 = selected;
        k.c.surface_base = surface;
        k.c.surface_weak = surface;
        k.c.surface_raised = surface;
        k.c.surface_raised_base = surface;
        k.c.surface_raised_hover = selected;
        k.c.surface_float = surface;
        k.c.surface_hover = selected;

        k.c.text_base = text;
        k.c.text_strong = text;
        k.c.text_muted = subtext;
        k.c.text_weak = subtext;
        k.c.text_faint = subtext;
        k.c.text_accent = accent;
        k.c.icon_base = text;
        k.c.icon_muted = subtext;
        k.c.icon_faint = subtext;
        k.c.icon_accent = accent;

        k.c.border_base = border;
        k.c.border_weak = border;
        k.c.border_muted = border;
        k.c.border_strong = selected;
        k.c.border_hover = selected;
        k.c.border_focus = accent;
        k.c.hover = selected.alpha(0.55);
        k.c.pressed = selected;
        k.c.scrim = Color::BLACK.alpha(0.55);
        k.c.bg_accent = accent;
        k.c.submit_fg = bg;
        k.c.submit_icon = bg;

        k.c.success = StateColors {
            bg: self.c(self.green).alpha(0.16),
            fg: self.c(self.green),
            border: self.c(self.green).alpha(0.4),
            icon: self.c(self.green),
        };
        k.c.warning = StateColors {
            bg: self.c(self.yellow).alpha(0.16),
            fg: self.c(self.yellow),
            border: self.c(self.yellow).alpha(0.4),
            icon: self.c(self.yellow),
        };
        k.c.danger = StateColors {
            bg: self.c(self.red).alpha(0.16),
            fg: self.c(self.red),
            border: self.c(self.red).alpha(0.4),
            icon: self.c(self.red),
        };
        k.c.info = StateColors {
            bg: accent.alpha(0.16),
            fg: accent,
            border: accent.alpha(0.4),
            icon: accent,
        };
        k
    }

    fn read() -> Option<Self> {
        let text = std::fs::read_to_string(colors_file()?).ok()?;
        let colors = parse(&text);
        let d = Self::default();
        let get = |key: &str, fallback: u32| colors.get(key).copied().unwrap_or(fallback);
        Some(Self {
            bg: get("background", d.bg),
            surface: get("dark_background", d.surface),
            border: get("lighter_background", d.border),
            selected: get("selection", d.selected),
            muted: get("muted", d.muted),
            text: get("foreground", d.text),
            subtext: get("light_foreground", d.subtext),
            accent: get("accent", d.accent),
            green: get("green", d.green),
            red: get("red", d.red),
            yellow: get("yellow", d.yellow),
            lanes: [
                get("blue", d.lanes[0]),
                get("green", d.lanes[1]),
                get("yellow", d.lanes[2]),
                get("red", d.lanes[3]),
                get("magenta", d.lanes[4]),
                get("cyan", d.lanes[5]),
                get("orange", d.lanes[6]),
                get("light_foreground", d.lanes[7]),
            ],
        })
    }
}

fn colors_file() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("omarchy/current/theme/colors.toml"))
}

/// Reads the flat `key = "#rrggbb"` lines of a `colors.toml`; anything else is ignored.
fn parse(text: &str) -> HashMap<String, u32> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let hex = value.trim().trim_matches('"').strip_prefix('#')?;
            if hex.len() != 6 {
                return None;
            }
            Some((key.trim().to_owned(), u32::from_str_radix(hex, 16).ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors_and_skips_other_lines() {
        let colors = parse("mode = \"dark\"\n# comment\naccent = \"#7fbbb3\"\nbad = \"#12\"\n");
        assert_eq!(colors.get("accent"), Some(&0x7fbbb3));
        assert_eq!(colors.len(), 1);
    }
}
