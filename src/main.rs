mod git;
mod graph;
mod icons;
mod input;
mod theme;

use anyhow::{Context as _, Result};
use git::{
    BranchInfo, Commit, CommitDetail, DiffLine, FileDiff, LineKind, RefKind, Snapshot,
    StashInfo,
};
use git2::Oid;
use gpui::{
    App, Application, Bounds, ClickEvent, ClipboardItem, Context, Corner, ElementId, Entity,
    FocusHandle, Focusable, Hsla, IntoElement, KeyBinding, ListHorizontalSizingBehavior, MouseButton, PathBuilder,
    PathPromptOptions, Pixels, Point, Render, ScrollStrategy, SharedString, Stateful, TitlebarOptions,
    UniformListScrollHandle, Window, WindowBounds, WindowOptions, actions, anchored, canvas, deferred,
    div, fill, point, prelude::*, px, rgb, rgba, size, uniform_list,
};
use icons::{Icon, icon};
use input::{InputColors, InputEvent, TextInput};
use graph::{Graph, GraphRow};
use std::{path::PathBuf, sync::Arc};
use theme::Theme;

actions!(gitbro, [SelectPrev, SelectNext, CloseDetail, OpenSelected, ToggleRecent, OpenRepo]);

const ROW_H: f32 = 28.0;
const LANE_W: f32 = 16.0;
const GRAPH_PAD: f32 = 10.0;
const NODE_R: f32 = 4.5;
const SIDEBAR_W: f32 = 240.0;
const FILES_SIDEBAR_W: f32 = 320.0;
const BRANCH_MENU_W: f32 = 200.0;
const ROW_BUTTON: f32 = 26.0;
const STATUS_H: f32 = 28.0;
const MAX_RECENT: usize = 10;

enum Status {
    Idle,
    Busy(String),
    Done(String),
    Failed(String),
}

#[derive(Clone, PartialEq)]
struct BranchRef {
    name: String,
    remote: bool,
}

/// What an open ⋯ menu belongs to.
#[derive(Clone, PartialEq)]
enum MenuTarget {
    Branch(BranchRef),
    Stash(usize),
}

#[derive(Clone)]
enum DialogKind {
    /// Branches off HEAD, which is the branch named `from`.
    NewBranch { from: String },
    Stash,
    /// Throws the uncommitted changes of `files` files away; `merging` ends an open merge too.
    Discard { files: usize, merging: bool },
    /// `unique` is the number of commits only this branch has.
    DeleteBranch { name: String, unique: usize },
    DropStash { index: usize, message: String },
}

/// A modal question; `input` is the text field of dialogs that ask for a name.
struct Dialog {
    kind: DialogKind,
    input: Option<Entity<TextInput>>,
}

enum DetailData {
    Loading,
    Ready(Arc<CommitDetail>),
    Failed(String),
}

/// The commit whose diff fills the main area.
struct DetailView {
    oid: Oid,
    data: DetailData,
    file: usize,
    files_scroll: UniformListScrollHandle,
    diff_scroll: UniformListScrollHandle,
}

struct GitBro {
    snapshot: Option<Snapshot>,
    graph: Graph,
    selected: Option<Oid>,
    selected_branch: Option<BranchRef>,
    status: Status,
    scroll: UniformListScrollHandle,
    theme: Theme,
    detail: Option<DetailView>,
    recent: Vec<PathBuf>,
    recent_open: bool,
    menu: Option<MenuTarget>,
    dialog: Option<Dialog>,
    commit_input: Entity<TextInput>,
    commit_focused: bool,
    /// The next commit replaces the last one instead of adding a new one.
    amend: bool,
    /// The last commit's message, if it was put into the commit field for the amend.
    amend_prefill: Option<String>,
    /// A folder that was opened but isn't a Git repository yet: offer to create one.
    no_repo: Option<PathBuf>,
    status_open: bool,
    focus: FocusHandle,
    mono: SharedString,
}

impl GitBro {
    fn new(start: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let commit_input = cx.new(|cx| TextInput::new(cx, "Commit-Nachricht (erste Zeile ist der Titel)", (4, 4)));
        // The commit button follows what is typed.
        cx.observe(&commit_input, |_, _, cx| cx.notify()).detach();
        let this = Self {
            snapshot: None,
            graph: Graph { rows: Vec::new(), width: 1 },
            selected: None,
            selected_branch: None,
            status: Status::Idle,
            scroll: UniformListScrollHandle::new(),
            theme: Theme::current(),
            detail: None,
            recent: load_recent(),
            recent_open: false,
            menu: None,
            dialog: None,
            commit_input,
            commit_focused: false,
            amend: false,
            amend_prefill: None,
            no_repo: None,
            status_open: false,
            focus: cx.focus_handle(),
            mono: mono_font(),
        };
        this.watch_theme(cx);
        if let Some(path) = start {
            this.reload(path, cx);
        }
        this
    }

    /// Hooks that need the window: re-read the repo when it is focused again, and let Esc leave
    /// the commit field.
    fn wire(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh(cx);
            }
        })
        .detach();
        let input = self.commit_input.clone();
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Submit => this.commit(cx),
            InputEvent::Cancel => {
                window.focus(&this.focus);
                cx.notify();
            }
        })
        .detach();
    }

    /// Omarchy swaps the theme files in place, so re-read them periodically and repaint on change.
    fn watch_theme(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                let theme = cx
                    .background_executor()
                    .spawn(async { Theme::current() })
                    .await;
                let alive = this.update(cx, |this, cx| {
                    if this.theme != theme {
                        this.theme = theme;
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn path(&self) -> Option<PathBuf> {
        self.snapshot.as_ref().map(|s| s.path.clone())
    }

    /// A new message always starts collapsed to a single line.
    fn set_status(&mut self, status: Status) {
        self.status_open = false;
        self.status = status;
    }

    fn busy(&self) -> bool {
        matches!(self.status, Status::Busy(_))
    }

    fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.no_repo = None;
        self.graph = graph::layout(&snapshot.commits);
        let known = |oid: Oid| snapshot.commits.iter().any(|c| c.oid == oid);
        self.selected = self.selected.filter(|&oid| known(oid));
        if self.detail.as_ref().is_some_and(|d| !known(d.oid)) {
            self.detail = None;
        }
        if let Some(sel) = &self.selected_branch {
            let list = if sel.remote { &snapshot.remote } else { &snapshot.local };
            if !list.iter().any(|b| b.name == sel.name) {
                self.selected_branch = None;
            }
        }
        if self.recent.first() != Some(&snapshot.path) {
            self.recent.retain(|p| *p != snapshot.path);
            self.recent.insert(0, snapshot.path.clone());
            self.recent.truncate(MAX_RECENT);
            save_recent(&self.recent);
        }
        if let Some(MenuTarget::Stash(index)) = self.menu
            && index >= snapshot.stashes.len()
        {
            self.menu = None;
        }
        self.snapshot = Some(snapshot);
    }

    fn reload(&self, path: PathBuf, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { git::load(&path) })
                .await;
            this.update(cx, |this, cx| {
                match loaded {
                    Ok(snapshot) => {
                        this.set_snapshot(snapshot);
                        this.set_status(Status::Idle);
                    }
                    Err(err) => match err.downcast_ref::<git::NotARepo>() {
                        Some(git::NotARepo(dir)) => {
                            // Nothing of the previously open repository may stay on screen.
                            this.snapshot = None;
                            this.graph = Graph { rows: Vec::new(), width: 1 };
                            this.detail = None;
                            this.selected = None;
                            this.selected_branch = None;
                            this.no_repo = Some(dir.clone());
                            this.set_status(Status::Idle);
                        }
                        None => this.set_status(Status::Failed(format!("{err:#}"))),
                    },
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Runs a git operation off the UI thread, then refreshes the whole view.
    fn run(
        &mut self,
        label: &str,
        cx: &mut Context<Self>,
        op: impl FnOnce(&std::path::Path) -> Result<String> + Send + 'static,
    ) {
        self.run_then(label, cx, op, |_, _, _| {});
    }

    /// Like [`Self::run`], and calls `after` with whether the operation succeeded.
    fn run_then(
        &mut self,
        label: &str,
        cx: &mut Context<Self>,
        op: impl FnOnce(&std::path::Path) -> Result<String> + Send + 'static,
        after: impl FnOnce(&mut Self, bool, &mut Context<Self>) + 'static,
    ) {
        let Some(path) = self.path() else { return };
        if self.busy() {
            return;
        }
        self.set_status(Status::Busy(label.to_owned()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (result, loaded) = cx
                .background_executor()
                .spawn(async move {
                    let result = op(&path);
                    (result, git::load(&path))
                })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(snapshot) = loaded {
                    this.set_snapshot(snapshot);
                    this.reload_wip_detail(cx);
                }
                let ok = result.is_ok();
                this.set_status(match result {
                    Ok(out) if out.is_empty() => Status::Done("Fertig".into()),
                    Ok(out) => Status::Done(out),
                    Err(err) => Status::Failed(format!("{err:#}")),
                });
                after(this, ok, cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let message = self.commit_input.read(cx).text().trim().to_owned();
        if message.is_empty() {
            return self.fail("Die Commit-Nachricht ist leer.", cx);
        }
        let amend = self.amend;
        self.run_then(
            if amend { "Ändere letzten Commit…" } else { "Committe…" },
            cx,
            move |p| git::commit(p, &message, amend),
            move |this, ok, cx| {
                if !ok {
                    return;
                }
                this.commit_input.update(cx, |input, cx| input.clear(cx));
                this.amend = false;
                this.amend_prefill = None;
                // A rewritten commit that is already on the remote can only be replaced there.
                this.run("Pushe…", cx, move |p| {
                    git::push(p, amend).context("Der Commit ist angelegt, aber der Push ist fehlgeschlagen")
                });
            },
        );
    }

    /// The amend switch: starts the message from the last commit's, unless something is typed.
    fn set_amend(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        if !on {
            self.end_amend(cx);
            return cx.notify();
        }
        let Some(path) = self.path() else { return };
        match git::head_message(&path) {
            Ok(message) => {
                if self.commit_input.read(cx).text().trim().is_empty() {
                    self.commit_input.update(cx, |input, cx| input.set_text(&message, cx));
                    self.amend_prefill = Some(message);
                }
                self.amend = true;
                cx.notify();
            }
            Err(err) => self.fail(&format!("{err:#}"), cx),
        }
    }

    /// Leaves amend mode and takes the borrowed message out of the field again, but never
    /// something the user wrote or edited.
    fn end_amend(&mut self, cx: &mut Context<Self>) {
        self.amend = false;
        if let Some(prefill) = self.amend_prefill.take()
            && self.commit_input.read(cx).text() == prefill
        {
            self.commit_input.update(cx, |input, cx| input.clear(cx));
        }
    }

    fn fail(&mut self, message: &str, cx: &mut Context<Self>) {
        self.set_status(Status::Failed(message.to_owned()));
        cx.notify();
    }

    fn open_dialog(&mut self, kind: DialogKind, window: &mut Window, cx: &mut Context<Self>) {
        let field = match &kind {
            DialogKind::NewBranch { .. } => Some("Name des Branches"),
            DialogKind::Stash => Some("Name (optional)"),
            DialogKind::Discard { .. } | DialogKind::DeleteBranch { .. } | DialogKind::DropStash { .. } => None,
        };
        let input = field.map(|placeholder| {
            let input = cx.new(|cx| TextInput::new(cx, placeholder, (1, 1)));
            cx.observe(&input, |_, _, cx| cx.notify()).detach();
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Submit => this.confirm_dialog(window, cx),
                InputEvent::Cancel => this.close_dialog(window, cx),
            })
            .detach();
            input
        });
        match &input {
            Some(input) => window.focus(&input.focus_handle(cx)),
            None => window.focus(&self.focus),
        }
        self.menu = None;
        self.recent_open = false;
        self.dialog = Some(Dialog { kind, input });
        cx.notify();
    }

    fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn confirm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = &self.dialog else { return };
        let kind = dialog.kind.clone();
        let text = dialog.input.as_ref().map(|i| i.read(cx).text().trim().to_owned()).unwrap_or_default();
        // Git ref names can't hold spaces, and typing one is far more likely a word break.
        let name = text.split_whitespace().collect::<Vec<_>>().join("-");
        if matches!(kind, DialogKind::NewBranch { .. }) && name.is_empty() {
            return;
        }
        self.close_dialog(window, cx);
        match kind {
            DialogKind::NewBranch { .. } => {
                self.run("Lege Branch an…", cx, move |p| git::create_branch(p, &name));
            }
            DialogKind::Discard { .. } => {
                self.run("Verwerfe Änderungen…", cx, |p| git::discard_all(p));
            }
            DialogKind::Stash => {
                self.run("Stashe Änderungen…", cx, move |p| git::stash_push(p, &text));
            }
            DialogKind::DeleteBranch { name, .. } => {
                self.run("Lösche Branch…", cx, move |p| git::delete_branch(p, &name));
            }
            DialogKind::DropStash { index, .. } => {
                self.run("Lösche Stash…", cx, move |p| git::stash_drop(p, index));
            }
        }
    }

    /// "Neuer Branch…" in the ⋯ menu of the current branch: the new one starts right here.
    fn ask_new_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(from) = self.head_name().map(str::to_owned) else { return };
        self.open_dialog(DialogKind::NewBranch { from }, window, cx);
    }

    fn ask_discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.snapshot else { return };
        let files = match self.detail.as_ref().map(|v| &v.data) {
            Some(DetailData::Ready(d)) if d.wip => d.files_total,
            _ => return,
        };
        let merging = snapshot.merging;
        self.open_dialog(DialogKind::Discard { files, merging }, window, cx);
    }

    fn ask_delete_branch(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path() else { return };
        match git::unique_commits(&path, &name) {
            Ok(unique) => self.open_dialog(DialogKind::DeleteBranch { name, unique }, window, cx),
            Err(err) => self.fail(&format!("{err:#}"), cx),
        }
    }

    fn open_repo(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Repository öffnen".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update(cx, |this, cx| this.open_path(path, cx)).ok();
        })
        .detach();
    }

    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.selected = None;
        self.selected_branch = None;
        self.detail = None;
        self.recent_open = false;
        self.menu = None;
        self.set_status(Status::Busy("Lade Repository…".into()));
        self.reload(path, cx);
        cx.notify();
    }

    fn select_branch(&mut self, branch: &BranchInfo, remote: bool, cx: &mut Context<Self>) {
        self.selected_branch = Some(BranchRef { name: branch.name.clone(), remote });
        if let Some(ix) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.commits.iter().position(|c| c.oid == branch.tip))
        {
            self.select_commit(ix, cx);
        }
        cx.notify();
    }

    /// Highlights a row in the graph and scrolls it into view.
    fn select_commit(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(commit) = self.snapshot.as_ref().and_then(|s| s.commits.get(ix)) else { return };
        self.selected = Some(commit.oid);
        self.scroll.scroll_to_item(ix, ScrollStrategy::Center);
        cx.notify();
    }

    fn commit_index(&self, oid: Oid) -> Option<usize> {
        self.snapshot.as_ref()?.commits.iter().position(|c| c.oid == oid)
    }

    /// Opens the full-screen diff of a commit and loads it in the background.
    fn open_commit(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.snapshot else { return };
        let Some(commit) = snapshot.commits.get(ix) else { return };
        let (oid, path) = (commit.oid, snapshot.path.clone());
        self.select_commit(ix, cx);
        self.recent_open = false;
        if self.detail.as_ref().is_some_and(|d| d.oid == oid) {
            return;
        }
        if oid != git::wip_oid() {
            self.end_amend(cx);
        }
        self.detail = Some(DetailView {
            oid,
            data: DetailData::Loading,
            file: 0,
            files_scroll: UniformListScrollHandle::new(),
            diff_scroll: UniformListScrollHandle::new(),
        });
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { git::load_detail(&path, oid) })
                .await;
            this.update(cx, |this, cx| {
                // The user may have moved on while this was loading.
                let Some(view) = this.detail.as_mut().filter(|d| d.oid == oid) else { return };
                view.data = match loaded {
                    Ok(detail) => DetailData::Ready(Arc::new(detail)),
                    Err(err) => DetailData::Failed(format!("{err:#}")),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn init_repo(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.set_status(Status::Busy("Lege Git-Repository an…".into()));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (created, loaded) = cx
                .background_executor()
                .spawn(async move { (git::init(&dir), git::load(&dir)) })
                .await;
            this.update(cx, |this, cx| {
                match (created, loaded) {
                    (Ok(_), Ok(snapshot)) => {
                        this.set_snapshot(snapshot);
                        this.set_status(Status::Done("Git-Repository angelegt.".into()));
                    }
                    (Err(err), _) | (_, Err(err)) => {
                        this.set_status(Status::Failed(format!("{err:#}")));
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Re-reads the repository, e.g. when the window regains focus after editing files elsewhere.
    /// Leaves the status line alone and does nothing while an operation is running.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        let Some(path) = self.path() else { return };
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { git::load(&path) })
                .await;
            this.update(cx, |this, cx| {
                if let Ok(snapshot) = loaded
                    && !this.busy()
                {
                    this.set_snapshot(snapshot);
                    this.reload_wip_detail(cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-reads the open working-tree diff in place, keeping the selected file.
    fn reload_wip_detail(&mut self, cx: &mut Context<Self>) {
        let wip = git::wip_oid();
        let Some(path) = self.path() else { return };
        let Some(view) = self.detail.as_ref().filter(|d| d.oid == wip) else { return };
        let current = match &view.data {
            DetailData::Ready(d) => d.files.get(view.file).map(|f| f.path.clone()),
            _ => None,
        };
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { git::load_detail(&path, wip) })
                .await;
            this.update(cx, |this, cx| {
                let Some(view) = this.detail.as_mut().filter(|d| d.oid == wip) else { return };
                if let Ok(detail) = loaded {
                    let kept = current.as_ref().and_then(|path| detail.files.iter().position(|f| f.path == *path));
                    if kept.is_none() {
                        view.diff_scroll = UniformListScrollHandle::new();
                    }
                    view.file = kept.unwrap_or_else(|| view.file.min(detail.files.len().saturating_sub(1)));
                    view.data = DetailData::Ready(Arc::new(detail));
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return self.confirm_dialog(window, cx);
        }
        if let Some(dir) = self.no_repo.clone() {
            return self.init_repo(dir, cx);
        }
        if self.detail.is_none()
            && let Some(ix) = self.selected.and_then(|oid| self.commit_index(oid))
        {
            self.open_commit(ix, cx);
        }
    }

    /// In the graph, up/down moves the highlighted commit; in the diff view it moves between files.
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.detail.is_some() {
            return self.step_file(delta, cx);
        }
        let Some(len) = self.snapshot.as_ref().map(|s| s.commits.len()).filter(|&n| n > 0) else {
            return;
        };
        let ix = match self.selected.and_then(|oid| self.commit_index(oid)) {
            Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
            None => 0,
        };
        self.select_commit(ix, cx);
    }

    fn step_file(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(view) = &mut self.detail else { return };
        let len = match &view.data {
            DetailData::Ready(d) if !d.files.is_empty() => d.files.len(),
            _ => return,
        };
        let next = (view.file as isize + delta).clamp(0, len as isize - 1) as usize;
        if next != view.file {
            view.file = next;
            view.diff_scroll = UniformListScrollHandle::new();
            view.files_scroll.scroll_to_item(next, ScrollStrategy::Center);
            cx.notify();
        }
    }

    /// Opens the neighbouring commit in the graph order (negative = newer).
    fn open_adjacent(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(ix) = self.detail.as_ref().and_then(|d| self.commit_index(d.oid)) else { return };
        let next = ix as isize + delta;
        if next >= 0 {
            self.open_commit(next as usize, cx);
        }
    }

    /// Esc: closes a dialog or menu first, then the status details, then the diff view.
    fn close_detail(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return self.close_dialog(window, cx);
        }
        if self.recent_open || self.menu.is_some() {
            self.recent_open = false;
            self.menu = None;
        } else if self.status_open {
            self.status_open = false;
        } else {
            self.detail = None;
            self.end_amend(cx);
        }
        cx.notify();
    }

    fn select_file(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(view) = &mut self.detail {
            view.file = ix;
            view.diff_scroll = UniformListScrollHandle::new();
            cx.notify();
        }
    }

    fn close_menus(&mut self, cx: &mut Context<Self>) {
        self.recent_open = false;
        self.menu = None;
        cx.notify();
    }

    fn head_name(&self) -> Option<&str> {
        self.snapshot.as_ref()?.head_branch.as_deref()
    }
}

impl Render for GitBro {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let colors = InputColors {
            text: t.text,
            hint: t.muted,
            caret: t.accent,
            selection: (t.accent << 8) | 0x55,
        };
        self.commit_input.update(cx, |input, _| input.set_colors(colors));
        if let Some(input) = self.dialog.as_ref().and_then(|d| d.input.as_ref()) {
            input.update(cx, |input, _| input.set_colors(colors));
        }
        // The commit field only exists in the working-tree diff; don't leave the keyboard focus
        // on it once that view is gone.
        let commit_visible = self
            .detail
            .as_ref()
            .is_some_and(|v| matches!(&v.data, DetailData::Ready(d) if d.wip));
        self.commit_focused = self.commit_input.focus_handle(cx).is_focused(window);
        if self.commit_focused && !commit_visible {
            window.focus(&self.focus);
            self.commit_focused = false;
        }
        // The WIP can vanish under an amend, e.g. when its changes were reverted elsewhere.
        if self.amend && !commit_visible {
            self.end_amend(cx);
        }
        let diff_open = self.detail.is_some();
        let main = if diff_open {
            self.diff_view(cx)
        } else {
            let main_w = window.viewport_size().width - px(SIDEBAR_W);
            self.history(main_w, cx).into_any_element()
        };
        div()
            .key_context("GitBro")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &CloseDetail, window, cx| this.close_detail(window, cx)))
            .on_action(cx.listener(|this, _: &OpenSelected, window, cx| this.open_selected(window, cx)))
            .on_action(cx.listener(|this, _: &OpenRepo, _, cx| this.open_repo(cx)))
            .on_action(cx.listener(|this, _: &ToggleRecent, _, cx| {
                this.recent_open = !this.recent_open;
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| window.focus(&this.focus)),
            )
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(t.bg))
            .text_color(rgb(t.text))
            .text_sm()
            .child(self.toolbar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.sidebar(cx))
                    .child(main),
            )
            .child(self.status_bar(cx))
            .children(self.menu_backdrop(cx))
            .children(self.recent_menu(cx))
            .children(self.dialog_view(cx))
    }
}

impl GitBro {
    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let has_repo = self.snapshot.is_some();
        let busy = self.busy();
        let merging = self.snapshot.as_ref().is_some_and(|s| s.merging);
        let name = self
            .snapshot
            .as_ref()
            .map(|s| &s.path)
            .or(self.no_repo.as_ref())
            .and_then(|p| p.file_name())
            .map_or("Repository öffnen".to_owned(), |n| n.to_string_lossy().into_owned());
        let branch = self.head_name().map(str::to_owned);
        let (ahead, behind) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.local.iter().find(|b| b.is_head))
            .map_or((0, 0), |b| (b.ahead, b.behind));

        div()
            .flex()
            .items_center()
            .gap_1()
            .h(px(40.))
            .px_2()
            .flex_none()
            .bg(rgb(t.surface))
            .border_b_1()
            .border_color(rgb(t.border))
            .child(activate(tool_button(t, "repo", Icon::Folder, &name)
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(icon(Icon::ChevronDown, 12., t.subtext)),
                true,
                t.border,
                cx.listener(|this, _, _, cx| {
                    this.recent_open = true;
                    this.menu = None;
                    cx.notify();
                }),
            ))
            .children(branch.map(|b| {
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .h(px(24.))
                    .px_2()
                    .bg(rgb(t.border))
                    .text_color(rgb(t.accent))
                    .child(icon(Icon::Branch, 13., t.accent))
                    .child(b)
            }))
            .child(div().flex_1())
            .when(merging, |d| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .mr_2()
                        .text_color(rgb(t.yellow))
                        .child(icon(Icon::Warning, 14., t.yellow))
                        .child("Merge unterbrochen"),
                )
                .child(activate(tool_button(t, "abort", Icon::X, "Abbrechen"),
                    !busy,
                    t.border,
                    cx.listener(|this, _, _, cx| {
                        this.run("Breche Merge ab…", cx, |p| git::abort_merge(p))
                    }),
                ))
            })
            .child(activate(tool_button(t, "pull", Icon::Pull, "Pull")
                    .children((behind > 0).then(|| count_badge(format!("↓{behind}"), t.accent))),
                has_repo && !busy,
                t.border,
                cx.listener(|this, _, _, cx| this.run("Pull…", cx, |p| git::sync(p))),
            ))
            .child(activate(tool_button(t, "push", Icon::Push, "Push")
                    .children((ahead > 0).then(|| count_badge(format!("↑{ahead}"), t.yellow))),
                has_repo && !busy,
                t.border,
                cx.listener(|this, _, _, cx| this.run("Push…", cx, |p| git::push(p, false))),
            ))
    }

    fn sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        if let Some(view) = &self.detail {
            return self.files_sidebar(view, cx);
        }
        let mut list = div()
            .id("sidebar")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .pb_2();

        if let Some(snapshot) = &self.snapshot {
            list = list.child(section_title(t, Icon::Branch, "Lokal", snapshot.local.len()));
            for b in &snapshot.local {
                list = list.child(self.branch_item(b, false, cx));
            }
            if !snapshot.stashes.is_empty() {
                list = list.child(section_title(t, Icon::Stash, "Stashes", snapshot.stashes.len()));
                for stash in &snapshot.stashes {
                    list = list.child(self.stash_item(stash, cx));
                }
            }
            // Remote branches are sorted by name, so each remote's branches are adjacent.
            let remote_of = |name: &str| name.split_once('/').map_or(name.to_owned(), |(r, _)| r.to_owned());
            let mut current: Option<String> = None;
            for b in &snapshot.remote {
                let remote = remote_of(&b.name);
                if current.as_deref() != Some(remote.as_str()) {
                    let count = snapshot.remote.iter().filter(|o| remote_of(&o.name) == remote).count();
                    list = list.child(section_title(t, Icon::Cloud, remote.clone(), count));
                    current = Some(remote);
                }
                list = list.child(self.branch_item(b, true, cx));
            }
        }
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(SIDEBAR_W))
            .h_full()
            .bg(rgb(t.surface))
            .border_r_1()
            .border_color(rgb(t.border))
            .child(list)
            .into_any_element()
    }

    /// Replaces the branch list while a diff is open: back button plus the commit's changed files.
    fn files_sidebar(&self, view: &DetailView, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let body = match &view.data {
            DetailData::Ready(d) => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(self.file_list(d, view, cx))
                .children(d.wip.then(|| self.commit_panel(d, cx)))
                .into_any_element(),
            DetailData::Loading => centered(t, "Lade…"),
            DetailData::Failed(_) => centered(t, "Fehler beim Laden"),
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(FILES_SIDEBAR_W))
            .h_full()
            .bg(rgb(t.surface))
            .border_r_1()
            .border_color(rgb(t.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(40.))
                    .px_2()
                    .child(primary_button(
                        t,
                        "back",
                        Icon::ArrowLeft,
                        "Verlauf",
                        true,
                        t.accent,
                        cx.listener(|this, _, window, cx| this.close_detail(window, cx)),
                    )),
            )
            .child(body)
            .into_any_element()
    }

    fn branch_item(&self, branch: &BranchInfo, remote: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let target = BranchRef { name: branch.name.clone(), remote };
        let selected = self.selected_branch.as_ref() == Some(&target);
        let menu_target = MenuTarget::Branch(target.clone());
        let menu_open = self.menu.as_ref() == Some(&menu_target);
        let current = branch.is_head && !remote;
        let key = format!("{}{}", if remote { "r:" } else { "l:" }, branch.name);
        // Remote branches sit under a heading with the remote's name.
        let label = if remote {
            branch.name.split_once('/').map_or(branch.name.as_str(), |(_, b)| b).to_owned()
        } else {
            branch.name.clone()
        };
        let mut badge = String::new();
        if branch.ahead > 0 {
            badge.push_str(&format!("↑{} ", branch.ahead));
        }
        if branch.behind > 0 {
            badge.push_str(&format!("↓{}", branch.behind));
        }
        let marker: Hsla = if current { rgb(t.accent).into() } else { gpui::transparent_black() };
        let group = SharedString::from(key.clone());
        let clicked = branch.clone();
        let menu = menu_open.then(|| self.branch_menu_view(&target, cx));

        div()
            .id(ElementId::Name(SharedString::from(key.clone())))
            .group(group.clone())
            .flex()
            .items_center()
            .h(px(ROW_BUTTON))
            .border_l_2()
            .border_color(marker)
            .when(selected, |d| d.bg(rgb(t.selected)))
            .hover(move |d| d.bg(rgb(t.border)))
            .child(
                div()
                    .id(ElementId::Name(SharedString::from(format!("{key}:main"))))
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .pl_2p5()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.select_branch(&clicked, remote, cx);
                        if event.click_count() >= 2 && !clicked.is_head && !this.busy() {
                            let name = clicked.name.clone();
                            this.run("Checkout…", cx, move |p| git::checkout(p, &name, remote));
                        }
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(current, |d| d.font_weight(gpui::FontWeight::BOLD))
                            .child(label),
                    )
                    .child(div().flex_none().text_xs().text_color(rgb(t.yellow)).child(badge)),
            )
            .child(self.row_menu_trigger(&key, group, menu_target, menu, cx))
    }

    /// The hover-only ⋯ button at the end of a sidebar row. An open menu hangs off it.
    fn row_menu_trigger(
        &self,
        key: &str,
        group: SharedString,
        target: MenuTarget,
        menu: Option<gpui::AnyElement>,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let t = self.theme;
        let menu_open = menu.is_some();
        div()
            .relative()
            .flex_none()
            .size(px(ROW_BUTTON))
            .child(
                div()
                    .id(ElementId::Name(SharedString::from(format!("{key}:more"))))
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .opacity(if menu_open { 1.0 } else { 0.0 })
                    .group_hover(group, |s| s.opacity(1.0))
                    .hover(move |d| d.bg(rgb(t.selected)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = Some(target.clone());
                        this.recent_open = false;
                        cx.notify();
                    }))
                    .child(icon(Icon::More, 14., t.text)),
            )
            .children(menu)
    }

    fn stash_item(&self, stash: &StashInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let target = MenuTarget::Stash(stash.index);
        let key = format!("s:{}", stash.index);
        let group = SharedString::from(key.clone());
        // Git writes "WIP on main: abc1234 subject" or "On main: your name".
        let label = stash
            .message
            .strip_prefix("WIP on ")
            .or_else(|| stash.message.strip_prefix("On "))
            .unwrap_or(&stash.message)
            .to_owned();
        let menu = (self.menu.as_ref() == Some(&target)).then(|| self.stash_menu_view(stash, cx));

        div()
            .id(ElementId::Name(SharedString::from(key.clone())))
            .group(group.clone())
            .flex()
            .items_center()
            .h(px(ROW_BUTTON))
            .border_l_2()
            .border_color(gpui::transparent_black())
            .hover(move |d| d.bg(rgb(t.border)))
            .child(div().flex_1().min_w_0().truncate().pl_2p5().child(label))
            .child(self.row_menu_trigger(&key, group, target, menu, cx))
    }

    /// The ⋯ menu of a branch row. It floats above everything, flush with the row's right edge.
    fn branch_menu_view(&self, target: &BranchRef, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let busy = self.busy();
        let head = self.head_name().map(str::to_owned);
        let checkout = target.clone();
        let source = target.name.clone();
        let doomed = target.name.clone();

        // The current branch is where new branches start; there is nothing else to do with it here.
        if !target.remote && head.as_deref() == Some(target.name.as_str()) {
            return popup(menu_panel(t, "branch-menu").child(menu_item(
                t,
                "menu-new-branch",
                Icon::Branch,
                "Neuer Branch…",
                !busy,
                cx.listener(|this, _, window, cx| {
                    this.menu = None;
                    this.ask_new_branch(window, cx);
                }),
            )));
        }
        let mut menu = menu_panel(t, "branch-menu").child(menu_item(
            t,
            "menu-checkout",
            Icon::Checkout,
            "Auschecken",
            !busy,
            cx.listener(move |this, _, _, cx| {
                this.menu = None;
                let b = checkout.clone();
                this.run("Checkout…", cx, move |p| git::checkout(p, &b.name, b.remote));
                cx.notify();
            }),
        ));
        if let Some(head) = head {
            menu = menu.child(menu_item(
                t,
                "menu-merge",
                Icon::Merge,
                format!("In {head} mergen"),
                !busy,
                cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    let source = source.clone();
                    this.run("Merge…", cx, move |p| git::merge(p, &source));
                    cx.notify();
                }),
            ));
        }
        // Remote branches belong to the remote; only local ones can be deleted from here.
        if !target.remote {
            menu = menu.child(divider(t)).child(menu_item(
                t,
                "menu-delete",
                Icon::Trash,
                "Löschen…",
                !busy,
                cx.listener(move |this, _, window, cx| {
                    this.menu = None;
                    this.ask_delete_branch(doomed.clone(), window, cx);
                }),
            ));
        }
        popup(menu)
    }

    fn stash_menu_view(&self, stash: &StashInfo, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let busy = self.busy();
        let (index, message) = (stash.index, stash.message.clone());
        popup(
            menu_panel(t, "stash-menu")
                .child(menu_item(
                    t,
                    "stash-apply",
                    Icon::Unstash,
                    "Anwenden",
                    !busy,
                    cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.run("Wende Stash an…", cx, move |p| git::stash_apply(p, index));
                        cx.notify();
                    }),
                ))
                .child(menu_item(
                    t,
                    "stash-pop",
                    Icon::Unstash,
                    "Entnehmen",
                    !busy,
                    cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.run("Wende Stash an…", cx, move |p| git::stash_pop(p, index));
                        cx.notify();
                    }),
                ))
                .child(divider(t))
                .child(menu_item(
                    t,
                    "stash-drop",
                    Icon::Trash,
                    "Löschen…",
                    !busy,
                    cx.listener(move |this, _, window, cx| {
                        let kind = DialogKind::DropStash { index, message: message.clone() };
                        this.open_dialog(kind, window, cx);
                    }),
                )),
        )
    }

    /// A click anywhere outside an open menu only closes it, like in any desktop app.
    fn menu_backdrop(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.recent_open && self.menu.is_none() {
            return None;
        }
        Some(
            deferred(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .occlude()
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _, cx| this.close_menus(cx)))
                    .on_mouse_down(MouseButton::Right, cx.listener(|this, _, _, cx| this.close_menus(cx))),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn history(&self, main_w: Pixels, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        if let Some(dir) = &self.no_repo {
            return self.init_prompt(dir.clone(), cx);
        }
        let Some(snapshot) = &self.snapshot else {
            return self.welcome(cx);
        };

        let count = snapshot.commits.len();
        let graph_w = GRAPH_PAD * 2.0 + LANE_W * self.graph.width as f32;
        // Columns give way to the description as the window gets narrower.
        let show_date = main_w >= px(640.);
        let show_author = main_w >= px(880.);
        let mono = self.mono.clone();
        let list = uniform_list(
            "commits",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let Some(snapshot) = &this.snapshot else { return Vec::new() };
                range
                    .map(|ix| {
                        let selected = this.selected == Some(snapshot.commits[ix].oid);
                        commit_row(
                            t,
                            mono.clone(),
                            ix,
                            &snapshot.commits[ix],
                            this.graph.rows[ix].clone(),
                            graph_w,
                            selected,
                            show_author,
                            show_date,
                            cx.listener(move |this, _, _, cx| this.open_commit(ix, cx)),
                        )
                    })
                    .collect()
            }),
        )
        .track_scroll(self.scroll.clone())
        .flex_1()
        .w_full();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .child(if count == 0 {
                centered(t, "Noch keine Commits.")
            } else {
                list.into_any_element()
            })
            .when(snapshot.truncated, |d| {
                d.child(
                    div()
                        .flex_none()
                        .px_3()
                        .py_1()
                        .text_xs()
                        .text_color(rgb(t.subtext))
                        .child(format!("Nur die neuesten {} Commits werden angezeigt.", git::MAX_COMMITS)),
                )
            })
            .into_any_element()
    }

    /// Shown for a folder that isn't a Git repository yet.
    fn init_prompt(&self, dir: PathBuf, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let busy = self.busy();
        let name = dir
            .file_name()
            .map_or_else(|| dir.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
        let full = dir.to_string_lossy().into_owned();
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .items_center()
            .justify_center()
            .gap_3()
            .px_6()
            .child(icon(Icon::Folder, 44., t.selected))
            .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child(name))
            .child(div().max_w_full().truncate().text_xs().text_color(rgb(t.subtext)).child(full))
            .child(
                div()
                    .text_color(rgb(t.subtext))
                    .child("Dieser Ordner ist noch kein Git-Repository."),
            )
            .child(activate(
                tool_button(t, "init", Icon::Branch, "Git Repository anlegen")
                    .h(px(32.))
                    .px_3()
                    .border_1()
                    .border_color(rgb(t.accent))
                    .text_color(rgb(t.accent)),
                !busy,
                t.border,
                cx.listener(move |this, _, _, cx| this.init_repo(dir.clone(), cx)),
            ))
            .child(activate(
                tool_button(t, "init-other", Icon::Folder, "Anderes Repository öffnen"),
                true,
                t.border,
                cx.listener(|this, _, _, cx| this.open_repo(cx)),
            ))
            .into_any_element()
    }

    /// Shown while no repository is open.
    fn welcome(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_3()
            .child(icon(Icon::Branch, 44., t.selected))
            .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child("GitBro"))
            .child(
                div()
                    .text_color(rgb(t.subtext))
                    .child("Öffne ein Repository, um den Verlauf zu sehen."),
            )
            .child(activate(tool_button(t, "open-big", Icon::Folder, "Repository öffnen")
                    .h(px(32.))
                    .px_3()
                    .border_1()
                    .border_color(rgb(t.selected)),
                true,
                t.border,
                cx.listener(|this, _, _, cx| this.open_repo(cx)),
            ))
            .when(!self.recent.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .mt_3()
                        .w(px(440.))
                        .max_w_full()
                        .border_1()
                        .border_color(rgb(t.border))
                        .child(
                            div()
                                .px_3()
                                .py_1()
                                .text_xs()
                                .text_color(rgb(t.subtext))
                                .child("ZULETZT GEÖFFNET"),
                        )
                        .children(self.recent_items(cx)),
                )
            })
            .into_any_element()
    }

    /// Full-area diff of the open commit; its changed files are listed in the sidebar.
    fn diff_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let pane = div().flex().flex_col().flex_1().min_w_0();
        let Some(view) = &self.detail else { return pane.into_any_element() };
        let ix = self.commit_index(view.oid);
        let len = self.snapshot.as_ref().map_or(0, |s| s.commits.len());
        let title = match &view.data {
            DetailData::Ready(d) => d.summary.clone(),
            _ => "Commit".to_owned(),
        };

        let title_bar = div()
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .h(px(40.))
            .pl_3()
            .pr_2()
            .bg(rgb(t.surface))
            .border_b_1()
            .border_color(rgb(t.border))
            .child(div().flex_1().min_w_0().truncate().font_weight(gpui::FontWeight::BOLD).child(title))
            .child(activate(tool_button(t, "newer", Icon::ChevronUp, "Neuer"),
                ix.is_some_and(|i| i > 0),
                t.border,
                cx.listener(|this, _, _, cx| this.open_adjacent(-1, cx)),
            ))
            .child(activate(tool_button(t, "older", Icon::ChevronDown, "Älter"),
                ix.is_some_and(|i| i + 1 < len),
                t.border,
                cx.listener(|this, _, _, cx| this.open_adjacent(1, cx)),
            ));

        let body = match &view.data {
            DetailData::Loading => centered(t, "Lade Commit…"),
            DetailData::Failed(err) => div()
                .p_3()
                .text_color(rgb(t.red))
                .child(err.clone())
                .into_any_element(),
            DetailData::Ready(d) => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .children((!d.wip).then(|| self.commit_meta(d, cx)))
                .child(self.diff_pane(d, view, cx))
                .into_any_element(),
        };
        pane.child(title_bar).child(body).into_any_element()
    }

    /// The repository switcher: recent projects plus "open another".
    fn recent_menu(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.recent_open {
            return None;
        }
        let t = self.theme;
        let menu = div()
            .id("recent-menu")
            .flex()
            .flex_col()
            .w(px(460.))
            .max_w_full()
            .bg(rgb(t.surface))
            .border_1()
            .border_color(rgb(t.selected))
            .shadow_md()
            .occlude()
            .children(self.recent_items(cx))
            .child(
                div()
                    .border_t_1()
                    .border_color(rgb(t.border))
                    .child(menu_item(
                        t,
                        "recent-open",
                        Icon::Plus,
                        "Anderes Repository öffnen…",
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.recent_open = false;
                            this.open_repo(cx);
                        }),
                    )),
            );
        Some(
            deferred(
                anchored()
                    .anchor(Corner::TopLeft)
                    .position(point(px(8.), px(40.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(menu),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    fn recent_items(&self, cx: &mut Context<Self>) -> Vec<Stateful<gpui::Div>> {
        let t = self.theme;
        self.recent
            .iter()
            .enumerate()
            .map(|(ix, path)| {
                let name = path
                    .file_name()
                    .map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
                let dir = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                let target = path.clone();
                div()
                    .id(("recent", ix))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(32.))
                    .px_3()
                    .cursor_pointer()
                    .hover(move |d| d.bg(rgb(t.selected)))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_path(target.clone(), cx)))
                    .child(icon(Icon::Folder, 14., t.subtext))
                    .child(div().flex_none().font_weight(gpui::FontWeight::BOLD).child(name))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(rgb(t.subtext))
                            .child(dir),
                    )
            })
            .collect()
    }

    fn commit_meta(&self, d: &Arc<CommitDetail>, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let full = d.id.clone();
        let parents = if d.parents.is_empty() {
            "Root-Commit".to_owned()
        } else {
            format!("Parent: {}", d.parents.join(", "))
        };
        div()
            .flex()
            .flex_col()
            .gap_1()
            .flex_none()
            .px_3()
            .py_2()
            .text_xs()
            .border_b_1()
            .border_color(rgb(t.border))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_x_4()
                    .text_color(rgb(t.subtext))
                    .child(
                        div()
                            .text_color(rgb(t.text))
                            .child(format!("{} <{}>", d.author, d.email)),
                    )
                    .child(d.time.clone())
                    .child(parents)
                    .child(
                        div()
                            .id("copy-hash")
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .cursor_pointer()
                            .font_family(self.mono.clone())
                            .text_color(rgb(t.accent))
                            .hover(move |s| s.bg(rgb(t.border)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(full.clone()));
                                this.set_status(Status::Done("Hash kopiert".into()));
                                cx.notify();
                            }))
                            .child(d.id[..10].to_owned())
                            .child(icon(Icon::Copy, 12., t.accent)),
                    ),
            )
            .when(!d.body.is_empty(), |el| {
                el.child(
                    div()
                        .id("commit-body")
                        .max_h(px(84.))
                        .overflow_y_scroll()
                        .text_sm()
                        .child(d.body.clone()),
                )
            })
    }

    fn file_list(
        &self,
        d: &Arc<CommitDetail>,
        view: &DetailView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let shown = d.files.len();
        let header = if d.files_total > shown {
            format!("{shown} von {} Dateien", d.files_total)
        } else if d.files_total == 1 {
            "1 Datei".to_owned()
        } else {
            format!("{} Dateien", d.files_total)
        };
        let list = uniform_list(
            "files",
            d.files.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                let Some(DetailView { data: DetailData::Ready(d), file, .. }) = &this.detail else {
                    return Vec::new();
                };
                range
                    .filter_map(|ix| {
                        let file_diff = d.files.get(ix)?;
                        Some(
                            file_row(
                                t,
                                ix,
                                file_diff,
                                *file == ix,
                                cx.listener(move |this, _, _, cx| this.select_file(ix, cx)),
                            )
                            .into_any_element(),
                        )
                    })
                    .collect()
            }),
        )
        .track_scroll(view.files_scroll.clone())
        .flex_1()
        .w_full();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .border_t_1()
            .border_color(rgb(t.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_none()
                    .h(px(24.))
                    .px_2()
                    .border_b_1()
                    .border_color(rgb(t.border))
                    .text_xs()
                    .text_color(rgb(t.subtext))
                    .child(div().flex_1().child(header))
                    .child(div().text_color(rgb(t.green)).child(format!("+{}", d.added)))
                    .child(div().text_color(rgb(t.red)).child(format!("−{}", d.removed))),
            )
            .child(list)
    }

    fn diff_pane(
        &self,
        d: &Arc<CommitDetail>,
        view: &DetailView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = self.theme;
        let pane = div().flex().flex_col().flex_1().min_w_0().min_h_0();
        let Some(file) = d.files.get(view.file) else {
            return pane.child(centered(t, "Keine Dateien geändert"));
        };

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .flex_none()
            .h(px(24.))
            .px_3()
            .border_b_1()
            .border_color(rgb(t.border))
            .text_xs()
            .child(div().flex_1().truncate().child(file.path.clone()))
            .child(div().text_color(rgb(t.green)).child(format!("+{}", file.added)))
            .child(div().text_color(rgb(t.red)).child(format!("−{}", file.removed)));

        let content = if file.binary {
            centered(t, "Binärdatei")
        } else if file.lines.is_empty() {
            centered(t, "Keine Textänderungen")
        } else {
            let list = uniform_list(
                ("diff", view.file),
                file.lines.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, _window, _cx| {
                    let Some(DetailView { data: DetailData::Ready(d), file, .. }) = &this.detail
                    else {
                        return Vec::new();
                    };
                    let lines = &d.files[*file].lines;
                    range.map(|ix| diff_row(t, &lines[ix])).collect()
                }),
            )
            .track_scroll(view.diff_scroll.clone())
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .with_width_from_item(Some(file.widest))
            .size_full();
            div()
                .flex_1()
                .min_h_0()
                .font_family(self.mono.clone())
                .child(list)
                .into_any_element()
        };
        pane.child(header).child(content)
    }

    /// Commit message and buttons, below the files of the working-tree diff. Committing takes
    /// every change and pushes right away.
    fn commit_panel(&self, d: &Arc<CommitDetail>, cx: &mut Context<Self>) -> gpui::AnyElement {
        let t = self.theme;
        let busy = self.busy();
        let snapshot = self.snapshot.as_ref();
        let has_remote = snapshot.is_some_and(|s| s.has_remote);
        let head = snapshot.and_then(|s| s.local.iter().find(|b| b.is_head));
        // Amend needs a commit to amend, and during a merge the next commit is the merge commit.
        let can_amend = head.is_some() && !snapshot.is_some_and(|s| s.merging);
        // An amended commit that the remote already has can only be replaced there.
        let rewrites_remote = self.amend && has_remote && head.is_some_and(|b| b.upstream && b.ahead == 0);
        let has_message = !self.commit_input.read(cx).text().trim().is_empty();
        let info = match d.files_total {
            1 => "1 Datei geändert".to_owned(),
            n => format!("{n} Dateien geändert"),
        };
        let label = match (self.amend, has_remote, rewrites_remote) {
            (false, false, _) => "Committen",
            (false, true, _) => "Committen & Pushen",
            (true, false, _) => "Amend",
            (true, true, false) => "Amend & Pushen",
            (true, true, true) => "Amend & Force-Push",
        };
        let amend_on = self.amend;
        div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_none()
            .p_2()
            .border_t_1()
            .border_color(rgb(t.border))
            .when(can_amend, |d| {
                d.child(
                    div()
                        .id("amend")
                        .flex()
                        .items_center()
                        .gap_2()
                        .h(px(22.))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| this.set_amend(!amend_on, cx)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .flex_none()
                                .size(px(16.))
                                .border_1()
                                .border_color(rgb(if amend_on { t.accent } else { t.subtext }))
                                .children(amend_on.then(|| div().size(px(8.)).bg(rgb(t.accent)))),
                        )
                        .child("Letzten Commit ändern (Amend)"),
                )
            })
            .child(
                div()
                    .border_1()
                    .border_color(rgb(if self.commit_focused { t.accent } else { t.border }))
                    .bg(rgb(t.bg))
                    .px_2()
                    .py_1()
                    .child(self.commit_input.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(t.subtext))
                    .child(div().flex_1().child(info))
                    .child("Strg+Enter"),
            )
            .when(rewrites_remote, |d| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(rgb(t.yellow))
                        .child("Der Commit liegt schon auf dem Remote. Der Push ersetzt ihn dort (force-with-lease)."),
                )
            })
            .child(primary_button(
                t,
                "commit",
                Icon::Commit,
                label,
                has_message && !busy,
                if rewrites_remote { t.yellow } else { t.accent },
                cx.listener(|this, _, _, cx| this.commit(cx)),
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(activate(
                        tool_button(t, "stash", Icon::Stash, "Stash")
                            .flex_1()
                            .justify_center()
                            .h(px(30.))
                            .border_1()
                            .border_color(rgb(t.border)),
                        !busy,
                        t.border,
                        cx.listener(|this, _, window, cx| this.open_dialog(DialogKind::Stash, window, cx)),
                    ))
                    .child(activate(
                        tool_button(t, "discard", Icon::Trash, "Verwerfen")
                            .flex_1()
                            .justify_center()
                            .h(px(30.))
                            .border_1()
                            .border_color(rgb(t.border)),
                        !busy,
                        t.border,
                        cx.listener(|this, _, window, cx| this.ask_discard(window, cx)),
                    )),
            )
            .into_any_element()
    }

    /// The modal question on top of everything; a click outside it cancels.
    fn dialog_view(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let dialog = self.dialog.as_ref()?;
        let t = self.theme;
        let (title, note, confirm, confirm_icon, danger) = match &dialog.kind {
            DialogKind::NewBranch { from } => (
                "Neuer Branch".to_owned(),
                format!("Startet bei „{from}“ und wird gleich ausgecheckt. Nicht committete Änderungen gehen mit."),
                "Anlegen",
                Icon::Branch,
                false,
            ),
            DialogKind::Stash => (
                "Änderungen stashen".to_owned(),
                "Alle Änderungen, auch neue Dateien, werden in einen Stash gelegt. Du findest ihn danach in der Sidebar unter „Stashes“."
                    .to_owned(),
                "Stashen",
                Icon::Stash,
                false,
            ),
            DialogKind::Discard { files, merging } => (
                "Alle Änderungen verwerfen?".to_owned(),
                format!(
                    "{} Der Stand springt auf den letzten Commit zurück, neue Dateien werden gelöscht.{} Das lässt sich nicht rückgängig machen.",
                    match files {
                        1 => "Eine Datei mit Änderungen:".to_owned(),
                        n => format!("{n} Dateien mit Änderungen:"),
                    },
                    if *merging { " Der offene Merge wird abgebrochen." } else { "" },
                ),
                "Verwerfen",
                Icon::Trash,
                true,
            ),
            DialogKind::DeleteBranch { name, unique } => (
                format!("Branch „{name}“ löschen?"),
                match unique {
                    0 => "Alles, was in diesem Branch steckt, ist auch anderswo erreichbar (anderer Branch, Remote oder Tag). Es geht nichts verloren.".to_owned(),
                    1 => "Ein Commit existiert nur in diesem Branch und geht mit dem Löschen verloren.".to_owned(),
                    n => format!("{n} Commits existieren nur in diesem Branch und gehen mit dem Löschen verloren."),
                },
                "Löschen",
                Icon::Trash,
                true,
            ),
            DialogKind::DropStash { message, .. } => (
                "Stash löschen?".to_owned(),
                format!("„{message}“ wird endgültig entfernt. Die darin gesicherten Änderungen gehen verloren."),
                "Löschen",
                Icon::Trash,
                true,
            ),
        };
        let ready = match &dialog.kind {
            DialogKind::NewBranch { .. } => {
                dialog.input.as_ref().is_some_and(|i| !i.read(cx).text().trim().is_empty())
            }
            _ => true,
        };
        let busy = self.busy();

        let card = div()
            .id("dialog")
            .flex()
            .flex_col()
            .gap_3()
            .w(px(440.))
            .max_w_full()
            .p_4()
            .bg(rgb(t.surface))
            .border_1()
            .border_color(rgb(t.selected))
            .shadow_md()
            // Clicks on the card itself must not reach the backdrop behind it.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().font_weight(gpui::FontWeight::BOLD).child(title))
            .child(div().text_color(rgb(t.subtext)).child(note))
            .children(dialog.input.clone().map(|input| {
                div()
                    .border_1()
                    .border_color(rgb(t.accent))
                    .bg(rgb(t.bg))
                    .px_2()
                    .py_1()
                    .child(input)
            }))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(activate(
                        tool_button(t, "dialog-cancel", Icon::X, "Abbrechen")
                            .h(px(30.))
                            .border_1()
                            .border_color(rgb(t.border)),
                        true,
                        t.border,
                        cx.listener(|this, _, window, cx| this.close_dialog(window, cx)),
                    ))
                    .child(primary_button(
                        t,
                        "dialog-ok",
                        confirm_icon,
                        confirm,
                        ready && !busy,
                        if danger { t.red } else { t.accent },
                        cx.listener(|this, _, window, cx| this.confirm_dialog(window, cx)),
                    )),
            );

        Some(
            deferred(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(rgba(0x00000088))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.close_dialog(window, cx)),
                    )
                    .child(card),
            )
            .with_priority(3)
            .into_any_element(),
        )
    }

    fn status_text(&self) -> (String, u32) {
        let t = self.theme;
        match &self.status {
            Status::Idle => (String::new(), t.subtext),
            Status::Busy(msg) => (msg.clone(), t.accent),
            Status::Done(msg) => (msg.clone(), t.green),
            Status::Failed(msg) => (msg.clone(), t.red),
        }
    }

    /// One line of fixed height. "Details" swaps the line for the full message and makes the
    /// bar itself taller (up to a limit, then it scrolls); collapsing restores the single line.
    fn status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = self.theme;
        let (text, color) = self.status_text();
        let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_owned();
        let has_more = matches!(self.status, Status::Done(_) | Status::Failed(_))
            && (text.trim().lines().count() > 1 || text.chars().count() > 100);
        let open = has_more && self.status_open;
        div()
            .id("status")
            .flex()
            .gap_3()
            .flex_none()
            .min_h(px(STATUS_H))
            .px_3()
            .bg(rgb(t.surface))
            .border_t_1()
            .border_color(rgb(t.border))
            .text_xs()
            .when(open, |d| d.items_start().py_2().max_h(px(240.)).overflow_y_scroll())
            .when(!open, |d| d.items_center().h(px(STATUS_H)))
            .when(has_more, |d| {
                d.cursor_pointer().on_click(cx.listener(|this, _, _, cx| {
                    this.status_open = !this.status_open;
                    cx.notify();
                }))
            })
            .child(if open {
                div().flex_1().min_w_0().text_color(rgb(color)).child(text.trim().to_owned())
            } else {
                div().flex_1().min_w_0().truncate().text_color(rgb(color)).child(first)
            })
            .when(has_more, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_color(rgb(t.subtext))
                        .child(if open { "Details ▾" } else { "Details ▴" }),
                )
            })
    }
}

fn centered(t: Theme, text: &'static str) -> gpui::AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_color(rgb(t.subtext))
        .child(text)
        .into_any_element()
}

/// One changed file.
fn file_row(
    t: Theme,
    ix: usize,
    file: &FileDiff,
    selected: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    let color = match file.status {
        'A' => t.green,
        'D' => t.red,
        'M' => t.yellow,
        _ => t.accent,
    };
    let path = std::path::Path::new(&file.path);
    let name = path
        .file_name()
        .map_or_else(|| file.path.clone(), |n| n.to_string_lossy().into_owned());
    let dir = match &file.old_path {
        Some(old) => format!("← {old}"),
        None => path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    div()
        .id(("file-row", ix))
        .flex()
        .items_center()
        .w_full()
        .h(px(26.))
        .px_2()
        .when(selected, |d| d.bg(rgb(t.selected)))
        .hover(move |d| d.bg(rgb(t.border)))
        .child(
            div()
                .id(("file", ix))
                .flex()
                .items_center()
                .gap_2()
                .flex_1()
                .min_w_0()
                .h_full()
                .cursor_pointer()
                .on_click(on_click)
                .child(
                    div()
                        .size(px(16.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .font_weight(gpui::FontWeight::BOLD)
                        .bg(rgba((color << 8) | 0x30))
                        .text_color(rgb(color))
                        .child(file.status.to_string()),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .child(div().flex_none().whitespace_nowrap().child(name))
                        .child(
                            div()
                                .truncate()
                                .text_xs()
                                .text_color(rgb(t.subtext))
                                .child(dir),
                        ),
                )
                .child(div().flex_none().text_xs().text_color(rgb(t.green)).child(format!("+{}", file.added)))
                .child(div().flex_none().text_xs().text_color(rgb(t.red)).child(format!("−{}", file.removed))),
        )
}

fn diff_row(t: Theme, line: &DiffLine) -> gpui::Div {
    let (tint, fg, sign) = match line.kind {
        LineKind::Add => (Some(rgba((t.green << 8) | 0x30)), t.text, "+"),
        LineKind::Del => (Some(rgba((t.red << 8) | 0x30)), t.text, "-"),
        LineKind::Hunk => (Some(rgba((t.accent << 8) | 0x20)), t.accent, ""),
        LineKind::Note => (None, t.subtext, ""),
        LineKind::Context => (None, t.text, " "),
    };
    let number = |n: Option<u32>| {
        div()
            .w(px(48.))
            .flex_none()
            .pr_2()
            .text_right()
            .text_color(rgb(t.subtext))
            .child(n.map(|n| n.to_string()).unwrap_or_default())
    };
    div()
        .flex()
        .items_center()
        .h(px(20.))
        .text_color(rgb(fg))
        .when_some(tint, |d, c| d.bg(c))
        .child(number(line.old))
        .child(number(line.new))
        .child(div().w(px(14.)).flex_none().child(sign))
        .child(div().flex_none().whitespace_nowrap().pr_4().child(line.text.clone()))
}

/// The monospace font Omarchy is configured with, used for diffs.
fn mono_font() -> SharedString {
    std::process::Command::new("omarchy")
        .args(["font", "current"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .map_or_else(|| "monospace".into(), Into::into)
}

fn section_title(t: Theme, icon_name: Icon, text: impl Into<SharedString>, count: usize) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .pt_3()
        .pb_1()
        .text_xs()
        .text_color(rgb(t.subtext))
        .child(icon(icon_name, 12., t.subtext))
        .child(div().flex_1().min_w_0().truncate().child(text.into()))
        .child(div().flex_none().child(count.to_string()))
}

/// Borderless toolbar-style button: icon and label. Wire it up with [`activate`].
fn tool_button(t: Theme, id: &'static str, icon_name: Icon, label: &str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1p5()
        .flex_none()
        .h(px(28.))
        .px_2()
        .child(icon(icon_name, 14., t.text))
        .child(label.to_owned())
}

fn count_badge(text: String, color: u32) -> gpui::Div {
    div().flex_none().text_xs().text_color(rgb(color)).child(text)
}

/// Filled call-to-action button in `color`; dimmed and inert when disabled.
fn primary_button(
    t: Theme,
    id: &'static str,
    icon_name: Icon,
    label: &'static str,
    enabled: bool,
    color: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    let button = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .gap_1p5()
        .h(px(30.))
        .px_3()
        .bg(rgb(color))
        .text_color(rgb(t.bg))
        .font_weight(gpui::FontWeight::BOLD)
        .child(icon(icon_name, 14., t.bg))
        .child(label);
    if enabled {
        button.cursor_pointer().hover(|d| d.opacity(0.85)).on_click(on_click)
    } else {
        button.opacity(0.4)
    }
}

/// The box of a ⋯ menu; fill it with [`menu_item`]s and hand it to [`popup`].
fn menu_panel(t: Theme, id: &'static str) -> Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .w(px(BRANCH_MENU_W))
        .py_1()
        .bg(rgb(t.surface))
        .border_1()
        .border_color(rgb(t.selected))
        .shadow_md()
        .occlude()
}

fn divider(t: Theme) -> gpui::Div {
    div().my_1().h(px(1.)).bg(rgb(t.border))
}

/// Floats a menu above everything, flush with the right edge of the row's ⋯ button.
fn popup(menu: Stateful<gpui::Div>) -> gpui::AnyElement {
    deferred(
        anchored()
            .anchor(Corner::TopLeft)
            // The popup is laid out after the ⋯ button, so it already starts at the row's
            // bottom edge; only the horizontal shift (right-align with the button) is needed.
            .offset(point(px(ROW_BUTTON - BRANCH_MENU_W), px(2.)))
            .snap_to_window_with_margin(px(8.))
            .child(menu),
    )
    .with_priority(2)
    .into_any_element()
}

/// Makes `el` clickable with a hover highlight, or dims it when disabled.
fn activate(
    el: Stateful<gpui::Div>,
    enabled: bool,
    hover: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    if enabled {
        el.cursor_pointer().hover(move |d| d.bg(rgb(hover))).on_click(on_click)
    } else {
        el.opacity(0.4)
    }
}

fn menu_item(
    t: Theme,
    id: &'static str,
    icon_name: Icon,
    label: impl Into<SharedString>,
    enabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    let item = div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .h(px(30.))
        .px_3()
        .child(icon(icon_name, 14., t.text))
        .child(div().min_w_0().truncate().child(label.into()));
    activate(item, enabled, t.selected, on_click)
}

#[allow(clippy::too_many_arguments)]
fn commit_row(
    t: Theme,
    mono: SharedString,
    ix: usize,
    commit: &Commit,
    row: GraphRow,
    graph_w: f32,
    selected: bool,
    show_author: bool,
    show_date: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<gpui::Div> {
    let wip = commit.wip;
    let local_names: Vec<&str> = commit
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Local)
        .map(|r| r.name.as_str())
        .collect();
    let short_remote = |name: &str| name.split_once('/').map_or(name.to_owned(), |(_, b)| b.to_owned());
    let refs = commit.refs.iter().filter_map(|r| {
        // `main` and `origin/main` on the same commit read as one label with a cloud mark.
        if r.kind == RefKind::Remote && local_names.contains(&short_remote(&r.name).as_str()) {
            return None;
        }
        let synced = r.kind == RefKind::Local
            && commit
                .refs
                .iter()
                .any(|o| o.kind == RefKind::Remote && short_remote(&o.name) == r.name);
        let pill = div()
            .flex()
            .items_center()
            .gap_1()
            .flex_none()
            .px_1p5()
            .mr_1()
            .text_xs()
            .when(r.is_head, |d| d.font_weight(gpui::FontWeight::BOLD));
        Some(match r.kind {
            RefKind::Local => pill
                .bg(rgb(t.accent))
                .text_color(rgb(t.bg))
                .child(r.name.clone())
                .when(synced, |d| d.child(icon(Icon::Cloud, 11., t.bg))),
            RefKind::Remote => pill
                .border_1()
                .border_color(rgb(t.green))
                .text_color(rgb(t.green))
                .child(icon(Icon::Cloud, 11., t.green))
                .child(r.name.clone()),
            RefKind::Tag => pill
                .bg(rgb(t.yellow))
                .text_color(rgb(t.bg))
                .child(icon(Icon::Tag, 11., t.bg))
                .child(r.name.clone()),
            RefKind::Head => pill.bg(rgb(t.red)).text_color(rgb(t.bg)).child(r.name.clone()),
        })
    });

    div()
        .id(("commit", ix))
        .flex()
        .items_center()
        .h(px(ROW_H))
        .w_full()
        .pr_3()
        .when(selected, |d| d.bg(rgb(t.selected)))
        .hover(move |d| d.bg(rgb(t.border)))
        .cursor_pointer()
        .on_click(on_click)
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| paint_graph_row(t, bounds, &row, wip, window),
            )
                .w(px(graph_w))
                .h(px(ROW_H))
                .flex_none(),
        )
        .child(
            div()
                .flex()
                .items_center()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .when(commit.wip, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .px_1p5()
                            .mr_1()
                            .text_xs()
                            .border_1()
                            .border_color(rgb(t.yellow))
                            .text_color(rgb(t.yellow))
                            .child("WIP"),
                    )
                })
                .children(refs)
                .child(
                    div()
                        .truncate()
                        .when(commit.wip, |d| d.text_color(rgb(t.subtext)))
                        .child(commit.summary.clone()),
                ),
        )
        .when(show_author, |d| {
            d.child(
                div()
                    .w(px(120.))
                    .flex_none()
                    .pl_2()
                    .truncate()
                    .text_color(rgb(t.subtext))
                    .child(commit.author.clone()),
            )
        })
        .when(show_date, |d| {
            d.child(
                div()
                    .w(px(140.))
                    .flex_none()
                    .pl_2()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_color(rgb(t.subtext))
                    .child(commit.time.clone()),
            )
        })
        .child(
            div()
                .w(px(64.))
                .flex_none()
                .pl_2()
                .text_xs()
                .font_family(mono)
                .text_color(rgb(t.subtext))
                .child(commit.short.clone()),
        )
}

fn paint_graph_row(
    t: Theme,
    bounds: gpui::Bounds<Pixels>,
    row: &GraphRow,
    wip: bool,
    window: &mut Window,
) {
    let lane_x = |lane: usize| bounds.origin.x + px(GRAPH_PAD + LANE_W * lane as f32 + LANE_W / 2.0);
    let top = bounds.origin.y;
    let mid = top + bounds.size.height / 2.0;
    let bottom = top + bounds.size.height;
    let color = |c: usize| -> Hsla { rgb(t.lanes[c % t.lanes.len()]).into() };
    let node_x = lane_x(row.lane);

    let mut stroke = |from: Point<Pixels>, to: Point<Pixels>, ctrl: Option<Point<Pixels>>, c: usize| {
        let mut builder = PathBuilder::stroke(px(2.0));
        builder.move_to(from);
        match ctrl {
            Some(ctrl) => builder.curve_to(to, ctrl),
            None => builder.line_to(to),
        }
        if let Ok(path) = builder.build() {
            window.paint_path(path, color(c));
        }
    };

    for &(lane, c) in &row.through {
        let x = lane_x(lane);
        stroke(point(x, top), point(x, bottom), None, c);
    }
    for &(lane, c) in &row.incoming {
        let x = lane_x(lane);
        let ctrl = (lane != row.lane).then(|| point(x, mid));
        stroke(point(x, top), point(node_x, mid), ctrl, c);
    }
    for &(lane, c) in &row.outgoing {
        let x = lane_x(lane);
        let ctrl = (lane != row.lane).then(|| point(x, mid));
        stroke(point(node_x, mid), point(x, bottom), ctrl, c);
    }

    let r = px(NODE_R);
    let node = gpui::Bounds::new(point(node_x - r, mid - r), size(r * 2.0, r * 2.0));
    if wip {
        // Uncommitted changes: an outlined node instead of a solid one.
        window.paint_quad(gpui::quad(
            node,
            0.0f32,
            gpui::transparent_black(),
            2.0f32,
            color(row.color),
            gpui::BorderStyle::Solid,
        ));
    } else {
        window.paint_quad(fill(node, color(row.color)));
    }
}

fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("gitbro"))
}

/// Recently opened repositories, most recent first; folders that no longer exist are dropped.
fn load_recent() -> Vec<PathBuf> {
    let Some(dir) = state_dir() else { return Vec::new() };
    // `last_repo` is where earlier versions kept the single last repository.
    let text = std::fs::read_to_string(dir.join("recent"))
        .or_else(|_| std::fs::read_to_string(dir.join("last_repo")))
        .unwrap_or_default();
    let mut list: Vec<PathBuf> = Vec::new();
    for path in text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| PathBuf::from(l).components().collect::<PathBuf>())
    {
        if path.is_dir() && !list.contains(&path) {
            list.push(path);
        }
    }
    list.truncate(MAX_RECENT);
    list
}

fn save_recent(list: &[PathBuf]) {
    let Some(dir) = state_dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let text: Vec<_> = list.iter().map(|p| p.to_string_lossy()).collect();
    let _ = std::fs::write(dir.join("recent"), text.join("\n"));
}

/// Repository to open at startup: CLI argument, else the last used one, else the current directory.
fn start_repo() -> Option<PathBuf> {
    if let Some(arg) = std::env::args_os().nth(1) {
        return Some(PathBuf::from(arg));
    }
    load_recent().into_iter().next().or_else(|| {
        let cwd = std::env::current_dir().ok()?;
        git2::Repository::discover(&cwd).ok().map(|_| cwd)
    })
}

fn main() {
    Application::new().with_assets(icons::Assets).run(|cx: &mut App| {
        input::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("down", SelectNext, Some("GitBro")),
            KeyBinding::new("up", SelectPrev, Some("GitBro")),
            KeyBinding::new("escape", CloseDetail, Some("GitBro")),
            KeyBinding::new("enter", OpenSelected, Some("GitBro")),
            KeyBinding::new("ctrl-r", ToggleRecent, Some("GitBro")),
            KeyBinding::new("ctrl-o", OpenRepo, Some("GitBro")),
        ]);
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("GitBro".into()),
                    ..Default::default()
                }),
                // Matches `StartupWMClass` and the icon name in assets/gitbro.desktop.
                app_id: Some("gitbro".into()),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| GitBro::new(start_repo(), cx));
                view.update(cx, |this, cx| this.wire(window, cx));
                window.focus(&view.read(cx).focus);
                view
            },
        )
        .unwrap();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
