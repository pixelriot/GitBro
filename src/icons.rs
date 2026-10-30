//! Small angular line icons, rendered by gpui as tinted masks.

use gpui::{AssetSource, Result, SharedString, Svg, Styled, px, rgb, svg};
use std::borrow::Cow;

#[derive(Clone, Copy)]
pub enum Icon {
    Pull,
    Push,
    Folder,
    Plus,
    Branch,
    Merge,
    Cloud,
    Tag,
    ChevronDown,
    ChevronUp,
    ArrowLeft,
    X,
    Copy,
    More,
    Warning,
    Checkout,
    Commit,
    Stash,
    Unstash,
    Trash,
}

const ALL: [Icon; 20] = [
    Icon::Pull,
    Icon::Push,
    Icon::Folder,
    Icon::Plus,
    Icon::Branch,
    Icon::Merge,
    Icon::Cloud,
    Icon::Tag,
    Icon::ChevronDown,
    Icon::ChevronUp,
    Icon::ArrowLeft,
    Icon::X,
    Icon::Copy,
    Icon::More,
    Icon::Warning,
    Icon::Checkout,
    Icon::Commit,
    Icon::Stash,
    Icon::Unstash,
    Icon::Trash,
];

impl Icon {
    pub fn path(self) -> &'static str {
        match self {
            Icon::Pull => "icons/pull.svg",
            Icon::Push => "icons/push.svg",
            Icon::Folder => "icons/folder.svg",
            Icon::Plus => "icons/plus.svg",
            Icon::Branch => "icons/branch.svg",
            Icon::Merge => "icons/merge.svg",
            Icon::Cloud => "icons/cloud.svg",
            Icon::Tag => "icons/tag.svg",
            Icon::ChevronDown => "icons/chevron-down.svg",
            Icon::ChevronUp => "icons/chevron-up.svg",
            Icon::ArrowLeft => "icons/arrow-left.svg",
            Icon::X => "icons/x.svg",
            Icon::Copy => "icons/copy.svg",
            Icon::More => "icons/more.svg",
            Icon::Warning => "icons/warning.svg",
            Icon::Checkout => "icons/checkout.svg",
            Icon::Commit => "icons/commit.svg",
            Icon::Stash => "icons/stash.svg",
            Icon::Unstash => "icons/unstash.svg",
            Icon::Trash => "icons/trash.svg",
        }
    }

    /// SVG elements on a 24x24 grid. Only the alpha matters: gpui tints the result.
    fn body(self) -> &'static str {
        match self {
            Icon::Pull => r#"<path d="M12 3v12"/><path d="M7 10l5 5 5-5"/><path d="M4 20h16"/>"#,
            Icon::Push => r#"<path d="M12 21V9"/><path d="M7 14l5-5 5 5"/><path d="M4 4h16"/>"#,
            Icon::Folder => r#"<path d="M3 5h7l2 3h9v11H3z"/>"#,
            Icon::Plus => r#"<path d="M12 5v14"/><path d="M5 12h14"/>"#,
            Icon::Branch => {
                r#"<path d="M6 3v12"/><rect x="3" y="15" width="6" height="6"/><rect x="15" y="3" width="6" height="6"/><path d="M18 9v3H6"/>"#
            }
            Icon::Merge => {
                r#"<rect x="3" y="3" width="6" height="6"/><rect x="15" y="15" width="6" height="6"/><path d="M6 9v9h9"/>"#
            }
            Icon::Cloud => r#"<path d="M7 18a5 5 0 0 1-.5-9.9 6 6 0 0 1 11.5 1.9A4 4 0 0 1 17 18z"/>"#,
            Icon::Tag => r#"<path d="M3 3h8l10 10-8 8L3 11z"/><path d="M7.5 7.5h.01"/>"#,
            Icon::ChevronDown => r#"<path d="M6 9l6 6 6-6"/>"#,
            Icon::ChevronUp => r#"<path d="M6 15l6-6 6 6"/>"#,
            Icon::ArrowLeft => r#"<path d="M19 12H5"/><path d="M11 6l-6 6 6 6"/>"#,
            Icon::X => r#"<path d="M6 6l12 12"/><path d="M18 6L6 18"/>"#,
            Icon::Copy => r#"<rect x="9" y="9" width="12" height="12"/><path d="M5 15H3V3h12v2"/>"#,
            Icon::More => {
                r##"<rect x="3" y="10.5" width="3.5" height="3.5" fill="#000" stroke="none"/><rect x="10.25" y="10.5" width="3.5" height="3.5" fill="#000" stroke="none"/><rect x="17.5" y="10.5" width="3.5" height="3.5" fill="#000" stroke="none"/>"##
            }
            Icon::Warning => r#"<path d="M12 3L2 21h20z"/><path d="M12 10v5"/><path d="M12 18v.01"/>"#,
            Icon::Checkout => r#"<path d="M10 4H4v16h6"/><path d="M9 12h12"/><path d="M16 7l5 5-5 5"/>"#,
            Icon::Commit => r#"<rect x="8" y="8" width="8" height="8"/><path d="M12 3v5"/><path d="M12 16v5"/>"#,
            Icon::Stash => r#"<path d="M3 4h18v5H3z"/><path d="M5 9v11h14V9"/><path d="M10 13h4"/>"#,
            Icon::Unstash => r#"<path d="M12 15V4"/><path d="M7 9l5-5 5 5"/><path d="M4 15v5h16v-5"/>"#,
            Icon::Trash => {
                r#"<path d="M4 7h16"/><path d="M9 7V4h6v3"/><path d="M6 7v13h12V7"/><path d="M10 11v6"/><path d="M14 11v6"/>"#
            }
        }
    }
}

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ALL.iter().find(|icon| icon.path() == path).map(|icon| {
            let svg = format!(
                r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="#000" stroke-width="2" stroke-linecap="square" stroke-linejoin="miter">{}</svg>"##,
                icon.body()
            );
            Cow::Owned(svg.into_bytes())
        }))
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(ALL.iter().map(|i| SharedString::from(i.path())).collect())
    }
}

pub fn icon(icon: Icon, size: f32, color: u32) -> Svg {
    svg().path(icon.path()).size(px(size)).flex_none().text_color(rgb(color))
}
