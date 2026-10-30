use std::{collections::HashMap, path::PathBuf};

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
