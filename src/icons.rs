//! Small angular line icons, rendered by Rune's SVG icon atlas and tinted with `.color()`.

use rune::Icon;

macro_rules! line_icon {
    ($name:ident, $key:literal, $body:literal) => {
        pub static $name: Icon = Icon {
            name: $key,
            view: 24.0,
            body: $body,
        };
    };
}

line_icon!(
    PULL,
    "gitbro-pull",
    r#"<path d="M12 3v12" stroke="currentColor" stroke-width="2"/><path d="M7 10l5 5 5-5" stroke="currentColor" stroke-width="2"/><path d="M4 20h16" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    PUSH,
    "gitbro-push",
    r#"<path d="M12 21V9" stroke="currentColor" stroke-width="2"/><path d="M7 14l5-5 5 5" stroke="currentColor" stroke-width="2"/><path d="M4 4h16" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    FOLDER,
    "gitbro-folder",
    r#"<path d="M3 5h7l2 3h9v11H3z" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    PLUS,
    "gitbro-plus",
    r#"<path d="M12 5v14" stroke="currentColor" stroke-width="2"/><path d="M5 12h14" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    BRANCH,
    "gitbro-branch",
    r#"<path d="M6 3v12" stroke="currentColor" stroke-width="2"/><rect x="3" y="15" width="6" height="6" stroke="currentColor" stroke-width="2"/><rect x="15" y="3" width="6" height="6" stroke="currentColor" stroke-width="2"/><path d="M18 9v3H6" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    MERGE,
    "gitbro-merge",
    r#"<rect x="3" y="3" width="6" height="6" stroke="currentColor" stroke-width="2"/><rect x="15" y="15" width="6" height="6" stroke="currentColor" stroke-width="2"/><path d="M6 9v9h9" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    CLOUD,
    "gitbro-cloud",
    r#"<path d="M7 18a5 5 0 0 1-.5-9.9 6 6 0 0 1 11.5 1.9A4 4 0 0 1 17 18z" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    TAG,
    "gitbro-tag",
    r#"<path d="M3 3h8l10 10-8 8L3 11z" stroke="currentColor" stroke-width="2"/><path d="M7.5 7.5h.01" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    CHEVRON_DOWN,
    "gitbro-chevron-down",
    r#"<path d="M6 9l6 6 6-6" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    CHEVRON_UP,
    "gitbro-chevron-up",
    r#"<path d="M6 15l6-6 6 6" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    ARROW_LEFT,
    "gitbro-arrow-left",
    r#"<path d="M19 12H5" stroke="currentColor" stroke-width="2"/><path d="M11 6l-6 6 6 6" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    X,
    "gitbro-x",
    r#"<path d="M6 6l12 12" stroke="currentColor" stroke-width="2"/><path d="M18 6L6 18" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    COPY,
    "gitbro-copy",
    r#"<rect x="9" y="9" width="12" height="12" stroke="currentColor" stroke-width="2"/><path d="M5 15H3V3h12v2" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    MORE,
    "gitbro-more",
    r#"<rect x="3" y="10.5" width="3.5" height="3.5" fill="currentColor" stroke="none"/><rect x="10.25" y="10.5" width="3.5" height="3.5" fill="currentColor" stroke="none"/><rect x="17.5" y="10.5" width="3.5" height="3.5" fill="currentColor" stroke="none"/>"#
);
line_icon!(
    WARNING,
    "gitbro-warning",
    r#"<path d="M12 3L2 21h20z" stroke="currentColor" stroke-width="2"/><path d="M12 10v5" stroke="currentColor" stroke-width="2"/><path d="M12 18v.01" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    CHECKOUT,
    "gitbro-checkout",
    r#"<path d="M10 4H4v16h6" stroke="currentColor" stroke-width="2"/><path d="M9 12h12" stroke="currentColor" stroke-width="2"/><path d="M16 7l5 5-5 5" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    COMMIT,
    "gitbro-commit",
    r#"<rect x="8" y="8" width="8" height="8" stroke="currentColor" stroke-width="2"/><path d="M12 3v5" stroke="currentColor" stroke-width="2"/><path d="M12 16v5" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    STASH,
    "gitbro-stash",
    r#"<path d="M3 4h18v5H3z" stroke="currentColor" stroke-width="2"/><path d="M5 9v11h14V9" stroke="currentColor" stroke-width="2"/><path d="M10 13h4" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    UNSTASH,
    "gitbro-unstash",
    r#"<path d="M12 15V4" stroke="currentColor" stroke-width="2"/><path d="M7 9l5-5 5 5" stroke="currentColor" stroke-width="2"/><path d="M4 15v5h16v-5" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    TRASH,
    "gitbro-trash",
    r#"<path d="M4 7h16" stroke="currentColor" stroke-width="2"/><path d="M9 7V4h6v3" stroke="currentColor" stroke-width="2"/><path d="M6 7v13h12V7" stroke="currentColor" stroke-width="2"/><path d="M10 11v6" stroke="currentColor" stroke-width="2"/><path d="M14 11v6" stroke="currentColor" stroke-width="2"/>"#
);
line_icon!(
    KEY,
    "gitbro-key",
    r#"<rect x="3" y="8" width="8" height="8" stroke="currentColor" stroke-width="2"/><path d="M11 12h10" stroke="currentColor" stroke-width="2"/><path d="M17 12v4" stroke="currentColor" stroke-width="2"/><path d="M21 12v3" stroke="currentColor" stroke-width="2"/>"#
);
