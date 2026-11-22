mod askpass;
mod git;
mod graph;
mod icons;
mod theme;

use anyhow::{Context as _, Result};
use askpass::Prompt;
use git::{BranchInfo, CommitDetail, DiffLine, FileDiff, LineKind, RefKind, Snapshot, StashInfo};
use git2::Oid;
use graph::{Graph, GraphRow};
use icons as ic;
use rune::{
    Align, AppSpec, Color, Cursor, Cx, El, GrowthSpec, Key, KeyEvent, LineCap, Path as DrawPath,
    ScrollSpec, Shape, TextEdit, TextStyle, Tx, Waker, canvas, col, el, icon, row, text,
};
use rune_kit::{Kit, Menu, MenuItem, Placement, TextArea, TextInput};
use std::{
    borrow::Cow,
    collections::VecDeque,
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc,
    time::Duration,
};
use theme::Theme;

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
    NewBranch {
        from: String,
    },
    Stash,
    /// Throws the uncommitted changes of `files` files away; `merging` ends an open merge too.
    Discard {
        files: usize,
        merging: bool,
    },
    /// `unique` is the number of commits only this branch has.
    DeleteBranch {
        name: String,
        unique: usize,
    },
    DropStash {
        index: usize,
        message: String,
    },
}

/// A modal question; `input` is the text field of dialogs that ask for a name.
struct DialogState {
    kind: DialogKind,
    input: Option<TextEdit>,
}

/// A login question from git or ssh. It sits above everything else until it is answered.
struct Login {
    request: askpass::Request,
    prompt: Prompt,
    /// The user name, asked for together with the password on `Prompt::Username`.
    user: TextEdit,
    answer: TextEdit,
}

enum DetailData {
    Loading,
    Ready(Rc<CommitDetail>),
    Failed(String),
}

/// The commit whose diff fills the main area.
struct DetailView {
    oid: Oid,
    data: DetailData,
    file: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum OpKind {
    Plain,
    Commit { amend: bool },
}

/// Results of work done on background threads.
enum Msg {
    Loaded {
        quiet: bool,
        result: Result<Snapshot>,
    },
    Detail {
        oid: Oid,
        result: Result<CommitDetail>,
    },
    WipDetail {
        /// Path of the file selected when the reload started.
        current: Option<String>,
        result: Result<CommitDetail>,
    },
    Op {
        kind: OpKind,
        result: Result<String>,
        loaded: Result<Snapshot>,
    },
    Init {
        created: Result<String>,
        loaded: Result<Snapshot>,
    },
    /// git or ssh wants a login; see `askpass`.
    Askpass(askpass::Request),
}

struct GitBro {
    snapshot: Option<Snapshot>,
    graph: Graph,
    selected: Option<Oid>,
    selected_branch: Option<BranchRef>,
    status: Status,
    theme: Theme,
    kit: Kit,
    detail: Option<DetailView>,
    recent: Vec<PathBuf>,
    recent_open: bool,
    menu: Option<MenuTarget>,
    dialog: Option<DialogState>,
    login: Option<Login>,
    /// Prompts that arrived while `login` was open.
    login_queue: VecDeque<askpass::Request>,
    /// The password typed with the user name, for git's password prompt that comes next:
    /// (host, password).
    login_password: Option<(String, String)>,
    /// The user cancelled a login during the running operation.
    login_cancelled: bool,
    commit: TextEdit,
    /// The next commit replaces the last one instead of adding a new one.
    amend: bool,
    /// The last commit's message, if it was put into the commit field for the amend.
    amend_prefill: Option<String>,
    /// A folder that was opened but isn't a Git repository yet: offer to create one.
    no_repo: Option<PathBuf>,
    status_open: bool,
    start: Option<PathBuf>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    waker: Option<Waker>,
    next_theme_check: f64,
    /// Bumped when a scroll viewport should forget its offset.
    history_epoch: u64,
    detail_epoch: u64,
    diff_epoch: u64,
    /// Bumped whenever commit row keys can change.
    history_version: u64,
}

fn init() -> GitBro {
    let (tx, rx) = mpsc::channel();
    let theme = Theme::current();
    GitBro {
        snapshot: None,
        graph: Graph {
            rows: Vec::new(),
            width: 1,
        },
        selected: None,
        selected_branch: None,
        status: Status::Idle,
        theme,
        kit: theme.kit(),
        detail: None,
        recent: load_recent(),
        recent_open: false,
        menu: None,
        dialog: None,
        login: None,
        login_queue: VecDeque::new(),
        login_password: None,
        login_cancelled: false,
        commit: TextEdit::new(""),
        amend: false,
        amend_prefill: None,
        no_repo: None,
        status_open: false,
        start: start_repo(),
        tx,
        rx,
        waker: None,
        next_theme_check: 0.0,
        history_epoch: 0,
        detail_epoch: 0,
        diff_epoch: 0,
        history_version: 0,
    }
}

fn setup(s: &mut GitBro, waker: Waker) {
    let tx = s.tx.clone();
    let wake = waker.clone();
    let listening = askpass::listen(move |request| {
        let _ = tx.send(Msg::Askpass(request));
        wake();
    })
    .map_err(anyhow::Error::from)
    .and_then(git::use_askpass);
    // Without the dialog git still runs; a missing login then fails with a hint instead.
    if let Err(err) = listening {
        eprintln!("gitbro: sign-in dialog unavailable: {err:#}");
    }
    s.waker = Some(waker);
    if let Some(path) = s.start.take() {
        s.reload(path, false);
    }
}

impl GitBro {
    fn path(&self) -> Option<PathBuf> {
        self.snapshot.as_ref().map(|s| s.path.clone())
    }

    fn busy(&self) -> bool {
        matches!(self.status, Status::Busy(_))
    }

    fn wip_visible(&self) -> bool {
        self.detail
            .as_ref()
            .is_some_and(|v| matches!(&v.data, DetailData::Ready(d) if d.wip))
    }

    /// A new message always starts collapsed to a single line.
    fn set_status(&mut self, status: Status) {
        self.status_open = false;
        self.status = status;
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.set_status(Status::Failed(message.into()));
    }

    fn history_scroll_key(&self) -> String {
        format!("history-scroll-{}", self.history_epoch)
    }

    fn files_scroll_key(&self) -> String {
        format!("files-scroll-{}", self.detail_epoch)
    }

    fn diff_scroll_key(&self) -> String {
        format!("diff-scroll-{}", self.diff_epoch)
    }

    fn set_snapshot(&mut self, snapshot: Snapshot) {
        self.no_repo = None;
        self.graph = graph::layout(&snapshot.commits);
        let known = |oid: Oid| snapshot.commits.iter().any(|c| c.oid == oid);
        self.selected = self.selected.filter(|&oid| known(oid));
        if self.detail.as_ref().is_some_and(|d| !known(d.oid)) {
            if self
                .detail
                .as_ref()
                .is_some_and(|d| d.oid == git::wip_oid())
            {
                self.end_amend();
                self.commit.focused = false;
            }
            self.detail = None;
        }
        if let Some(sel) = &self.selected_branch {
            let list = if sel.remote {
                &snapshot.remote
            } else {
                &snapshot.local
            };
            if !list.iter().any(|b| b.name == sel.name) {
                self.selected_branch = None;
            }
        }
        if let Some(MenuTarget::Branch(sel)) = &self.menu {
            let list = if sel.remote {
                &snapshot.remote
            } else {
                &snapshot.local
            };
            let stale = !list.iter().any(|b| b.name == sel.name);
            if stale {
                self.menu = None;
            }
        }
        // A refresh of the open repository must not put a removed entry back.
        // Recording happens when the open repository changes.
        let switching = self
            .snapshot
            .as_ref()
            .is_none_or(|current| current.path != snapshot.path);
        if switching {
            self.note_recent(&snapshot.path);
            self.history_epoch += 1;
            self.detail_epoch += 1;
            self.diff_epoch += 1;
        }
        if let Some(MenuTarget::Stash(index)) = self.menu
            && index >= snapshot.stashes.len()
        {
            self.menu = None;
        }
        self.snapshot = Some(snapshot);
        self.history_version += 1;
    }

    fn clear_repo(&mut self) {
        self.snapshot = None;
        self.graph = Graph {
            rows: Vec::new(),
            width: 1,
        };
        self.selected = None;
        self.selected_branch = None;
        self.detail = None;
        self.menu = None;
        self.end_amend();
        self.commit.focused = false;
        self.history_epoch += 1;
        self.detail_epoch += 1;
        self.diff_epoch += 1;
        self.history_version += 1;
    }

    /// Loads a repository off the UI thread. `quiet` refreshes leave the status line alone.
    fn reload(&self, path: PathBuf, quiet: bool) {
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let result = git::load(&path);
            let _ = tx.send(Msg::Loaded { quiet, result });
            if let Some(wake) = waker {
                wake();
            }
        });
    }

    /// Re-reads the repository, e.g. when the window regains focus after editing files elsewhere.
    fn refresh(&mut self) {
        if self.busy() {
            return;
        }
        let Some(path) = self.path() else { return };
        self.reload(path, true);
    }

    /// Runs a git operation off the UI thread, then refreshes the whole view.
    fn run(
        &mut self,
        label: &str,
        kind: OpKind,
        op: impl FnOnce(&Path) -> Result<String> + Send + 'static,
    ) {
        let Some(path) = self.path() else { return };
        if self.busy() {
            return;
        }
        self.set_status(Status::Busy(label.to_owned()));
        self.login_cancelled = false;
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let result = op(&path);
            let loaded = git::load(&path);
            let _ = tx.send(Msg::Op {
                kind,
                result,
                loaded,
            });
            if let Some(wake) = waker {
                wake();
            }
        });
    }

    fn do_commit(&mut self) {
        if self.busy() {
            return;
        }
        let message = self.commit.text.trim().to_owned();
        if message.is_empty() {
            return self.fail("The commit message is empty.");
        }
        let amend = self.amend;
        self.run(
            if amend {
                "Amending last commit…"
            } else {
                "Committing…"
            },
            OpKind::Commit { amend },
            move |p| git::commit(p, &message, amend),
        );
    }

    /// The amend switch: starts the message from the last commit's, unless something is typed.
    fn set_amend(&mut self, on: bool) {
        if self.busy() {
            return;
        }
        if !on {
            self.end_amend();
            return;
        }
        let Some(path) = self.path() else { return };
        match git::head_message(&path) {
            Ok(message) => {
                if self.commit.text.trim().is_empty() {
                    self.commit.replace_text(&message);
                    self.amend_prefill = Some(message);
                }
                self.amend = true;
            }
            Err(err) => self.fail(format!("{err:#}")),
        }
    }

    /// Leaves amend mode and takes the borrowed message out of the field again, but never
    /// something the user wrote or edited.
    fn end_amend(&mut self) {
        self.amend = false;
        if let Some(prefill) = self.amend_prefill.take()
            && self.commit.text == prefill
        {
            self.commit.clear();
        }
    }

    fn open_dialog(&mut self, kind: DialogKind) {
        let input = match &kind {
            DialogKind::NewBranch { .. } | DialogKind::Stash => {
                let mut edit = TextEdit::new("");
                edit.focused = true;
                Some(edit)
            }
            _ => None,
        };
        // The modal owns the keyboard while it is open.
        self.commit.focused = false;
        self.menu = None;
        self.recent_open = false;
        self.dialog = Some(DialogState { kind, input });
    }

    fn close_dialog(&mut self) {
        if let Some(dialog) = &mut self.dialog
            && let Some(input) = &mut dialog.input
        {
            input.focused = false;
        }
        self.dialog = None;
    }

    fn confirm_dialog(&mut self) {
        let Some(dialog) = &self.dialog else { return };
        let kind = dialog.kind.clone();
        let text = dialog
            .input
            .as_ref()
            .map(|i| i.text.trim().to_owned())
            .unwrap_or_default();
        // Git ref names can't hold spaces, and typing one is far more likely a word break.
        let name = text.split_whitespace().collect::<Vec<_>>().join("-");
        if matches!(kind, DialogKind::NewBranch { .. }) && name.is_empty() {
            return;
        }
        self.close_dialog();
        match kind {
            DialogKind::NewBranch { .. } => {
                self.run("Creating branch…", OpKind::Plain, move |p| {
                    git::create_branch(p, &name)
                });
            }
            DialogKind::Discard { .. } => {
                self.run("Discarding changes…", OpKind::Plain, git::discard_all);
            }
            DialogKind::Stash => {
                self.run("Stashing changes…", OpKind::Plain, move |p| {
                    git::stash_push(p, &text)
                });
            }
            DialogKind::DeleteBranch { name, .. } => {
                self.run("Deleting branch…", OpKind::Plain, move |p| {
                    git::delete_branch(p, &name)
                });
            }
            DialogKind::DropStash { index, .. } => {
                self.run("Deleting stash…", OpKind::Plain, move |p| {
                    git::stash_drop(p, index)
                });
            }
        }
    }

    /// "New branch…" in the ⋯ menu of the current branch: the new one starts right here.
    fn ask_new_branch(&mut self) {
        let Some(from) = self.head_name().map(str::to_owned) else {
            return;
        };
        self.open_dialog(DialogKind::NewBranch { from });
    }

    fn ask_discard(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let files = match self.detail.as_ref().map(|v| &v.data) {
            Some(DetailData::Ready(d)) if d.wip => d.files_total,
            _ => return,
        };
        let merging = snapshot.merging;
        self.open_dialog(DialogKind::Discard { files, merging });
    }

    fn ask_delete_branch(&mut self, name: String) {
        let Some(path) = self.path() else { return };
        match git::unique_commits(&path, &name) {
            Ok(unique) => self.open_dialog(DialogKind::DeleteBranch { name, unique }),
            Err(err) => self.fail(format!("{err:#}")),
        }
    }

    fn open_repo(&mut self, tx: &mut Tx) {
        let options = rune::FileDialogOptions {
            title: "Open repository".into(),
            ..Default::default()
        };
        tx.open_folder(options, |s: &mut GitBro, result, _tx| {
            if let Ok(Some(path)) = result {
                s.open_path(path);
            }
        });
    }

    fn open_path(&mut self, path: PathBuf) {
        self.selected = None;
        self.selected_branch = None;
        self.detail = None;
        self.recent_open = false;
        self.menu = None;
        self.end_amend();
        self.commit.focused = false;
        self.detail_epoch += 1;
        self.diff_epoch += 1;
        // Opening the repository already on screen is how a removed entry gets back on the list.
        if self
            .snapshot
            .as_ref()
            .is_some_and(|current| current.path == path)
        {
            self.note_recent(&path);
        }
        self.set_status(Status::Busy("Loading repository…".into()));
        self.reload(path, false);
    }

    /// Puts `path` at the front of the recent list and stores it.
    fn note_recent(&mut self, path: &Path) {
        if self.recent.first().is_some_and(|p| p == path) {
            return;
        }
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(MAX_RECENT);
        save_recent(&self.recent);
    }

    /// Drops `path` from the recent list. The repository on disk is left alone.
    fn forget_recent(&mut self, path: &Path) {
        let len = self.recent.len();
        self.recent.retain(|p| p != path);
        if self.recent.len() != len {
            save_recent(&self.recent);
        }
    }

    fn select_branch(&mut self, branch: &BranchInfo, remote: bool, tx: &mut Tx) {
        self.selected_branch = Some(BranchRef {
            name: branch.name.clone(),
            remote,
        });
        if let Some(ix) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.commits.iter().position(|c| c.oid == branch.tip))
        {
            self.select_commit(ix, tx);
        }
    }

    /// Highlights a row in the graph and scrolls it into view.
    fn select_commit(&mut self, ix: usize, tx: &mut Tx) {
        let Some(commit) = self.snapshot.as_ref().and_then(|s| s.commits.get(ix)) else {
            return;
        };
        self.selected = Some(commit.oid);
        let key = format!("commit-{}", commit.oid);
        tx.scroll_center_in(self.history_scroll_key(), key);
    }

    fn commit_index(&self, oid: Oid) -> Option<usize> {
        self.snapshot
            .as_ref()?
            .commits
            .iter()
            .position(|c| c.oid == oid)
    }

    /// Opens the full-screen diff of a commit and loads it in the background.
    fn open_commit(&mut self, ix: usize, tx: &mut Tx) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let Some(commit) = snapshot.commits.get(ix) else {
            return;
        };
        let (oid, path) = (commit.oid, snapshot.path.clone());
        self.select_commit(ix, tx);
        self.recent_open = false;
        if self.detail.as_ref().is_some_and(|d| d.oid == oid) {
            return;
        }
        if oid != git::wip_oid() {
            self.end_amend();
            self.commit.focused = false;
        }
        self.detail_epoch += 1;
        self.diff_epoch += 1;
        self.detail = Some(DetailView {
            oid,
            data: DetailData::Loading,
            file: 0,
        });
        let tx_msg = self.tx.clone();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let result = git::load_detail(&path, oid);
            let _ = tx_msg.send(Msg::Detail { oid, result });
            if let Some(wake) = waker {
                wake();
            }
        });
    }

    fn init_repo(&mut self, dir: PathBuf) {
        if self.busy() {
            return;
        }
        self.set_status(Status::Busy("Creating Git repository…".into()));
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let created = git::init(&dir);
            let loaded = git::load(&dir);
            let _ = tx.send(Msg::Init { created, loaded });
            if let Some(wake) = waker {
                wake();
            }
        });
    }

    /// Re-reads the open working-tree diff in place, keeping the selected file.
    fn reload_wip_detail(&mut self) {
        let wip = git::wip_oid();
        let Some(path) = self.path() else { return };
        let Some(view) = self.detail.as_ref().filter(|d| d.oid == wip) else {
            return;
        };
        let current = match &view.data {
            DetailData::Ready(d) => d.files.get(view.file).map(|f| f.path.clone()),
            _ => None,
        };
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        std::thread::spawn(move || {
            let result = git::load_detail(&path, wip);
            let _ = tx.send(Msg::WipDetail { current, result });
            if let Some(wake) = waker {
                wake();
            }
        });
    }

    fn open_selected(&mut self, tx: &mut Tx) {
        if self.dialog.is_some() {
            return self.confirm_dialog();
        }
        if let Some(dir) = self.no_repo.clone() {
            return self.init_repo(dir);
        }
        if self.detail.is_none()
            && let Some(ix) = self.selected.and_then(|oid| self.commit_index(oid))
        {
            self.open_commit(ix, tx);
        }
    }

    /// In the graph, up/down moves the highlighted commit; in the diff view it moves between files.
    fn move_selection(&mut self, delta: isize, tx: &mut Tx) {
        if self.detail.is_some() {
            return self.step_file(delta, tx);
        }
        let Some(len) = self
            .snapshot
            .as_ref()
            .map(|s| s.commits.len())
            .filter(|&n| n > 0)
        else {
            return;
        };
        let ix = match self.selected.and_then(|oid| self.commit_index(oid)) {
            Some(i) => (i as isize + delta).clamp(0, len as isize - 1) as usize,
            None => 0,
        };
        self.select_commit(ix, tx);
    }

    fn step_file(&mut self, delta: isize, tx: &mut Tx) {
        let Some(view) = &mut self.detail else { return };
        let len = match &view.data {
            DetailData::Ready(d) if !d.files.is_empty() => d.files.len(),
            _ => return,
        };
        let next = (view.file as isize + delta).clamp(0, len as isize - 1) as usize;
        if next != view.file {
            view.file = next;
            self.diff_epoch += 1;
            tx.scroll_center_in(self.files_scroll_key(), format!("file-{next}"));
        }
    }

    /// Opens the neighbouring commit in the graph order (negative = newer).
    fn open_adjacent(&mut self, delta: isize, tx: &mut Tx) {
        let Some(ix) = self.detail.as_ref().and_then(|d| self.commit_index(d.oid)) else {
            return;
        };
        let next = ix as isize + delta;
        if next >= 0 {
            self.open_commit(next as usize, tx);
        }
    }

    /// Esc: closes a dialog or menu first, then the status details, then the diff view.
    fn close_detail(&mut self, tx: &mut Tx) {
        if self.dialog.is_some() {
            return self.close_dialog();
        }
        if self.recent_open || self.menu.is_some() {
            self.recent_open = false;
            self.menu = None;
        } else if self.status_open {
            self.status_open = false;
        } else {
            self.detail = None;
            self.end_amend();
            self.commit.focused = false;
            if let Some(ix) = self.selected.and_then(|oid| self.commit_index(oid))
                && let Some(commit) = self.snapshot.as_ref().and_then(|s| s.commits.get(ix))
            {
                tx.scroll_center_in(self.history_scroll_key(), format!("commit-{}", commit.oid));
            }
        }
    }

    fn select_file(&mut self, ix: usize) {
        if let Some(view) = &mut self.detail {
            view.file = ix;
            self.diff_epoch += 1;
        }
    }

    fn close_menus(&mut self) {
        self.recent_open = false;
        self.menu = None;
    }

    /// Shows a login prompt from git or ssh, or queues it behind the one already open.
    fn ask_login(&mut self, request: askpass::Request) {
        if self.login.is_some() {
            self.login_queue.push_back(request);
            return;
        }
        let prompt = Prompt::parse(&request.prompt);
        // The password was typed into the same dialog as the user name.
        if let Prompt::Password { host, .. } = &prompt
            && self
                .login_password
                .as_ref()
                .is_some_and(|(for_host, _)| for_host == host)
        {
            let (_, password) = self.login_password.take().unwrap_or_default();
            request.answer(Some(password));
            return self.next_login();
        }
        let mut user = TextEdit::new("");
        let mut answer = TextEdit::new("");
        match prompt {
            Prompt::Username { .. } => user.focused = true,
            Prompt::TrustHost { .. } => {}
            _ => answer.focused = true,
        }
        // The login owns the keyboard while it is open.
        self.commit.focused = false;
        if let Some(input) = self.dialog.as_mut().and_then(|d| d.input.as_mut()) {
            input.focused = false;
        }
        self.menu = None;
        self.recent_open = false;
        self.login = Some(Login {
            request,
            prompt,
            user,
            answer,
        });
    }

    fn next_login(&mut self) {
        if let Some(request) = self.login_queue.pop_front() {
            self.ask_login(request);
        }
    }

    fn submit_login(&mut self) {
        let Some(login) = self.login.take() else {
            return;
        };
        let answer = match &login.prompt {
            Prompt::Username { host } => {
                let name = login.user.text.trim().to_owned();
                if name.is_empty() {
                    self.login = Some(login);
                    return;
                }
                self.login_password = Some((host.clone(), login.answer.text.clone()));
                name
            }
            Prompt::TrustHost { .. } => "yes".to_owned(),
            _ => login.answer.text.clone(),
        };
        login.request.answer(Some(answer));
        self.next_login();
    }

    /// Esc or Cancel: git gives up, and so does every prompt still waiting.
    fn cancel_login(&mut self) {
        let Some(login) = self.login.take() else {
            return;
        };
        login.request.answer(None);
        for request in self.login_queue.drain(..) {
            request.answer(None);
        }
        self.login_password = None;
        self.login_cancelled = true;
    }

    fn head_name(&self) -> Option<&str> {
        self.snapshot.as_ref()?.head_branch.as_deref()
    }
}

fn commit_edit(s: &mut GitBro) -> &mut TextEdit {
    &mut s.commit
}

fn dialog_edit(s: &mut GitBro) -> &mut TextEdit {
    s.dialog
        .as_mut()
        .and_then(|d| d.input.as_mut())
        .expect("a dialog with an input")
}

fn commit_field_key(s: &mut GitBro, ev: &KeyEvent, tx: &mut Tx) -> bool {
    // A focused Rune text field takes Command chords before the window's key handler.
    if ev.is_command('r') {
        s.recent_open = !s.recent_open;
        s.menu = None;
        return true;
    }
    if ev.is_command('o') {
        s.open_repo(tx);
        return true;
    }
    // A multi-line commit message: Return starts the next line, Ctrl/Cmd+Enter submits.
    if ev.key == Key::Enter {
        if ev.mods.cmd || ev.mods.ctrl {
            s.do_commit();
            return true;
        }
        if !ev.mods.shift && !ev.mods.alt {
            let mut next = ev.clone();
            next.mods.shift = true;
            s.commit.key(&next, None);
            return true;
        }
    }
    false
}

fn dialog_field_key(s: &mut GitBro, ev: &KeyEvent, _tx: &mut Tx) -> bool {
    if ev.key == Key::Escape {
        s.close_dialog();
        return true;
    }
    false
}

fn login_user(s: &mut GitBro) -> &mut TextEdit {
    &mut s.login.as_mut().expect("an open login").user
}

fn login_answer(s: &mut GitBro) -> &mut TextEdit {
    &mut s.login.as_mut().expect("an open login").answer
}

fn login_field_key(s: &mut GitBro, ev: &KeyEvent, _tx: &mut Tx) -> bool {
    if ev.key == Key::Escape {
        s.cancel_login();
        return true;
    }
    false
}

/// Return in the user name field moves on to the password, unless that is filled in already.
fn login_user_submit(s: &mut GitBro, _tx: &mut Tx) {
    let Some(login) = &mut s.login else { return };
    if login.answer.text.is_empty() && !login.user.text.trim().is_empty() {
        login.user.focused = false;
        login.answer.focused = true;
    } else {
        s.submit_login();
    }
}

fn on_key(s: &mut GitBro, ev: &KeyEvent, tx: &mut Tx) -> bool {
    if s.login.is_some() {
        return match ev.key {
            Key::Escape => {
                s.cancel_login();
                true
            }
            Key::Enter => {
                s.submit_login();
                true
            }
            _ => false,
        };
    }
    if ev.is_command('r') {
        s.recent_open = !s.recent_open;
        s.menu = None;
        return true;
    }
    if ev.is_command('o') {
        s.open_repo(tx);
        return true;
    }
    let plain = !ev.mods.cmd && !ev.mods.ctrl && !ev.mods.alt && !ev.mods.shift;
    if !plain {
        return false;
    }
    match ev.key {
        Key::Down => {
            s.move_selection(1, tx);
            true
        }
        Key::Up => {
            s.move_selection(-1, tx);
            true
        }
        Key::Escape => {
            s.close_detail(tx);
            true
        }
        Key::Enter => {
            s.open_selected(tx);
            true
        }
        _ => false,
    }
}

fn on_focus(s: &mut GitBro, focused: bool, _tx: &mut Tx) {
    if focused {
        s.refresh();
    }
}

fn poll(s: &mut GitBro, time: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    while let Ok(msg) = s.rx.try_recv() {
        changed |= handle(s, msg, tx);
    }
    if time >= s.next_theme_check {
        s.next_theme_check = time + 1.0;
        let theme = Theme::current();
        if theme != s.theme {
            s.theme = theme;
            s.kit = theme.kit();
            changed = true;
        }
        tx.wake_after(Duration::from_secs(1));
    }
    // The commit field only exists in the working-tree diff; don't leave keyboard focus on it
    // once that view is gone.
    if s.commit.focused && !s.wip_visible() {
        s.commit.focused = false;
        changed = true;
    }
    // The WIP can vanish under an amend, e.g. when its changes were reverted elsewhere.
    if s.amend && !s.wip_visible() {
        s.end_amend();
        changed = true;
    }
    changed
}

fn handle(s: &mut GitBro, msg: Msg, _tx: &mut Tx) -> bool {
    match msg {
        Msg::Loaded { quiet, result } => match result {
            Ok(snapshot) => {
                if quiet && s.busy() {
                    return false;
                }
                s.set_snapshot(snapshot);
                if !quiet {
                    s.set_status(Status::Idle);
                }
                true
            }
            Err(err) => {
                if quiet {
                    return false;
                }
                match err.downcast_ref::<git::NotARepo>() {
                    Some(git::NotARepo(dir)) => {
                        // Nothing of the previously open repository may stay on screen.
                        s.clear_repo();
                        s.no_repo = Some(dir.clone());
                        s.set_status(Status::Idle);
                    }
                    None => s.fail(format!("{err:#}")),
                }
                true
            }
        },
        Msg::Detail { oid, result } => {
            // The user may have moved on while this was loading.
            let Some(view) = s.detail.as_mut().filter(|d| d.oid == oid) else {
                return false;
            };
            view.data = match result {
                Ok(detail) => DetailData::Ready(Rc::new(detail)),
                Err(err) => DetailData::Failed(format!("{err:#}")),
            };
            true
        }
        Msg::WipDetail { current, result } => {
            let wip = git::wip_oid();
            let Ok(detail) = result else { return false };
            let kept = current
                .as_ref()
                .and_then(|path| detail.files.iter().position(|f| f.path == *path));
            let bump = kept.is_none();
            let old_file = s.detail.as_ref().map_or(0, |v| v.file);
            let Some(view) = s.detail.as_mut().filter(|d| d.oid == wip) else {
                return false;
            };
            view.file = kept.unwrap_or_else(|| old_file.min(detail.files.len().saturating_sub(1)));
            view.data = DetailData::Ready(Rc::new(detail));
            if bump {
                s.diff_epoch += 1;
            }
            true
        }
        Msg::Op {
            kind,
            result,
            loaded,
        } => {
            let cancelled = std::mem::take(&mut s.login_cancelled);
            s.login_password = None;
            if let Ok(snapshot) = loaded {
                s.set_snapshot(snapshot);
                s.reload_wip_detail();
            }
            let ok = result.is_ok();
            s.set_status(match result {
                Ok(out) if out.is_empty() => Status::Done("Done".into()),
                Ok(out) => Status::Done(out),
                // git's own message would only say that no login was given.
                Err(err) if cancelled => Status::Failed(if err.chain().count() > 1 {
                    format!("{err}: sign-in cancelled.")
                } else {
                    "Sign-in cancelled.".into()
                }),
                Err(err) => Status::Failed(format!("{err:#}")),
            });
            if ok && let OpKind::Commit { amend } = kind {
                s.commit.clear();
                s.amend = false;
                s.amend_prefill = None;
                // A rewritten commit that is already on the remote can only be replaced there.
                s.run("Pushing…", OpKind::Plain, move |p| {
                    git::push(p, amend).context("The commit was created, but the push failed")
                });
            }
            true
        }
        Msg::Init { created, loaded } => {
            match (created, loaded) {
                (Ok(_), Ok(snapshot)) => {
                    s.set_snapshot(snapshot);
                    s.set_status(Status::Done("Git repository created.".into()));
                }
                (Err(err), _) | (_, Err(err)) => s.fail(format!("{err:#}")),
            }
            true
        }
        Msg::Askpass(request) => {
            s.ask_login(request);
            true
        }
    }
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

fn view(s: &GitBro, cx: &mut Cx<GitBro>) -> El {
    let t = s.theme;
    let k = &s.kit;
    let mut layers: Vec<El> = Vec::new();

    let main = if s.detail.is_some() {
        diff_view(s, cx, k, t)
    } else {
        history(s, cx, k, t)
    };
    let sidebar = if s.detail.is_some() {
        files_sidebar(s, k, t)
    } else {
        branch_sidebar(s, k, t, &mut layers)
    };
    let base = col()
        .key("gitbro-root")
        .w_full()
        .h_full()
        .fill(t.c(t.bg))
        .child(toolbar(s, k, t))
        .child(
            row()
                .grow(1.0)
                .w_full()
                .min_h(0.0)
                .child(sidebar)
                .child(main),
        )
        .child(status_bar(s, k, t));

    if s.recent_open {
        recent_menu(s, k, t, &mut layers);
    }
    if s.dialog.is_some() {
        dialog_view(s, k, t, &mut layers);
    }
    if s.login.is_some() {
        login_view(s, k, t, &mut layers);
    }
    rune_kit::root(base, layers)
}

// ---------------------------------------------------------------------------
// Small shared pieces
// ---------------------------------------------------------------------------

fn xs(k: &Kit) -> TextStyle {
    TextStyle {
        size: 12.0,
        line_height: 16.0,
        ..k.ty.ui
    }
}

fn tiny(k: &Kit) -> TextStyle {
    TextStyle {
        size: 11.0,
        line_height: 14.0,
        ..k.ty.ui
    }
}

fn bold(style: TextStyle) -> TextStyle {
    TextStyle {
        weight: 700.0,
        ..style
    }
}

fn separator(t: Theme) -> El {
    el().w_full().h(1.0).fill(t.c(t.border))
}

fn vseparator(t: Theme) -> El {
    el().w(1.0).h_full().fill(t.c(t.border))
}

fn label(k: &Kit, value: impl Into<String>, color: Color) -> El {
    text(value).font(k.ty.ui).color(color)
}

fn centered(k: &Kit, t: Theme, value: &'static str) -> El {
    col()
        .grow(1.0)
        .w_full()
        .center()
        .child(text(value).font(k.ty.ui).color(t.c(t.subtext)))
}

fn activate(mut e: El, enabled: bool, hover: Color) -> El {
    if enabled {
        e = e.hover(hover).press(hover);
    } else {
        e = e.opacity(0.4);
    }
    e
}

fn tool_button(
    k: &Kit,
    t: Theme,
    icon_ref: &'static rune::Icon,
    value: impl Into<String>,
    enabled: bool,
    on_press: impl Fn(&mut GitBro) + 'static,
) -> El {
    tool_button_styled(
        k,
        t,
        icon_ref,
        value,
        enabled,
        k.ty.ui,
        t.c(t.text),
        on_press,
    )
}

fn tool_button_bold(
    k: &Kit,
    t: Theme,
    icon_ref: &'static rune::Icon,
    value: impl Into<String>,
    enabled: bool,
    on_press: impl Fn(&mut GitBro) + 'static,
) -> El {
    tool_button_styled(
        k,
        t,
        icon_ref,
        value,
        enabled,
        bold(k.ty.ui),
        t.c(t.text),
        on_press,
    )
}

fn tool_button_accent(
    k: &Kit,
    t: Theme,
    icon_ref: &'static rune::Icon,
    value: impl Into<String>,
    enabled: bool,
    on_press: impl Fn(&mut GitBro) + 'static,
) -> El {
    tool_button_styled(
        k,
        t,
        icon_ref,
        value,
        enabled,
        k.ty.ui,
        t.c(t.accent),
        on_press,
    )
}

#[allow(clippy::too_many_arguments)]
fn tool_button_styled(
    _k: &Kit,
    t: Theme,
    icon_ref: &'static rune::Icon,
    value: impl Into<String>,
    enabled: bool,
    style: TextStyle,
    color: Color,
    on_press: impl Fn(&mut GitBro) + 'static,
) -> El {
    let value = value.into();
    let mut e = row()
        .h(28.0)
        .px(8.0)
        .gap(6.0)
        .items_center()
        .role("button")
        .label(value.clone())
        .cursor(Cursor::Pointer)
        .child(icon(icon_ref, 14.0).color(color))
        .child(text(value).font(style).color(color));
    e = activate(e, enabled, t.c(t.selected));
    if enabled {
        e = e.on_press(on_press);
    }
    e
}

fn tool_button_tx(
    k: &Kit,
    t: Theme,
    icon_ref: &'static rune::Icon,
    value: impl Into<String>,
    enabled: bool,
    on_press: impl Fn(&mut GitBro, &mut Tx) + 'static,
) -> El {
    let value = value.into();
    let mut e = row()
        .h(28.0)
        .px(8.0)
        .gap(6.0)
        .items_center()
        .role("button")
        .label(value.clone())
        .cursor(Cursor::Pointer)
        .child(icon(icon_ref, 14.0).color(t.c(t.text)))
        .child(label(k, value, t.c(t.text)));
    e = activate(e, enabled, t.c(t.selected));
    if enabled {
        e = e.on_press_tx(on_press);
    }
    e
}

fn primary_button(
    k: &Kit,
    t: Theme,
    icon_ref: &'static rune::Icon,
    value: impl Into<String>,
    enabled: bool,
    color: Color,
) -> El {
    let value = value.into();
    let mut e = row()
        .h(30.0)
        .px(12.0)
        .gap(6.0)
        .items_center()
        .justify_center()
        .role("button")
        .label(value.clone())
        .cursor(Cursor::Pointer)
        .fill(color)
        .child(icon(icon_ref, 14.0).color(t.c(t.bg)))
        .child(text(value).font(bold(k.ty.ui)).color(t.c(t.bg)));
    if enabled {
        e = e.hover(color.mix(Color::WHITE, 0.08));
    } else {
        e = e.opacity(0.4);
    }
    e
}

fn count_badge(k: &Kit, value: String, color: Color) -> El {
    text(value).font(tiny(k)).color(color)
}

// ---------------------------------------------------------------------------
// Toolbar
// ---------------------------------------------------------------------------

fn toolbar(s: &GitBro, k: &Kit, t: Theme) -> El {
    let has_repo = s.snapshot.is_some();
    let busy = s.busy();
    let merging = s.snapshot.as_ref().is_some_and(|s| s.merging);
    let name = s
        .snapshot
        .as_ref()
        .map(|s| &s.path)
        .or(s.no_repo.as_ref())
        .and_then(|p| p.file_name())
        .map_or("Open repository".to_owned(), |n| {
            n.to_string_lossy().into_owned()
        });
    let branch = s.head_name().map(str::to_owned);
    let (ahead, behind) = s
        .snapshot
        .as_ref()
        .and_then(|s| s.local.iter().find(|b| b.is_head))
        .map_or((0, 0), |b| (b.ahead, b.behind));

    let repo_button = tool_button_bold(k, t, &ic::FOLDER, name, true, |s| {
        s.recent_open = true;
        s.menu = None;
    })
    .child(icon(&ic::CHEVRON_DOWN, 12.0).color(t.c(t.subtext)));

    let mut cols = vec![repo_button];
    if let Some(branch) = branch {
        cols.push(
            row()
                .h(24.0)
                .px(8.0)
                .gap(6.0)
                .items_center()
                .fill(t.c(t.border))
                .child(icon(&ic::BRANCH, 13.0).color(t.c(t.accent)))
                .child(
                    text(branch)
                        .font(k.ty.ui)
                        .color(t.c(t.accent))
                        .max_w(240.0)
                        .truncate(),
                ),
        );
    }
    cols.push(el().grow(1.0));

    if merging {
        cols.push(
            row()
                .mr(8.0)
                .gap(6.0)
                .items_center()
                .child(icon(&ic::WARNING, 14.0).color(t.c(t.yellow)))
                .child(text("Merge interrupted").font(k.ty.ui).color(t.c(t.yellow))),
        );
        cols.push(tool_button(k, t, &ic::X, "Abort", !busy, |s| {
            s.run("Aborting merge…", OpKind::Plain, git::abort_merge)
        }));
    }

    let mut pull = tool_button(k, t, &ic::PULL, "Pull", has_repo && !busy, |s| {
        s.run("Pull…", OpKind::Plain, git::sync)
    });
    if behind > 0 {
        pull = pull.child(count_badge(k, format!("↓{behind}"), t.c(t.accent)));
    }
    let mut push = tool_button(k, t, &ic::PUSH, "Push", has_repo && !busy, |s| {
        s.run("Push…", OpKind::Plain, move |p| git::push(p, false))
    });
    if ahead > 0 {
        push = push.child(count_badge(k, format!("↑{ahead}"), t.c(t.yellow)));
    }
    cols.push(pull);
    cols.push(push);

    col()
        .h(40.0)
        .w_full()
        .fill(t.c(t.surface))
        .child(
            row()
                .grow(1.0)
                .w_full()
                .px(8.0)
                .gap(4.0)
                .items_center()
                .children(cols),
        )
        .child(separator(t))
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

fn section_title(
    k: &Kit,
    t: Theme,
    icon_ref: &'static rune::Icon,
    value: impl Into<String>,
    count: usize,
) -> El {
    row()
        .px(12.0)
        .pt(12.0)
        .pb(4.0)
        .gap(8.0)
        .items_center()
        .child(icon(icon_ref, 12.0).color(t.c(t.subtext)))
        .child(
            text(value)
                .font(xs(k))
                .color(t.c(t.subtext))
                .grow(1.0)
                .truncate(),
        )
        .child(text(count.to_string()).font(xs(k)).color(t.c(t.subtext)))
}

fn branch_sidebar(s: &GitBro, k: &Kit, t: Theme, layers: &mut Vec<El>) -> El {
    let mut list = col().w_full().px(0.0).pb(8.0);

    if let Some(snapshot) = &s.snapshot {
        list = list.child(section_title(
            k,
            t,
            &ic::BRANCH,
            "Local",
            snapshot.local.len(),
        ));
        for b in &snapshot.local {
            list = list.child(branch_item(s, k, t, b, false, layers));
        }
        if !snapshot.stashes.is_empty() {
            list = list.child(section_title(
                k,
                t,
                &ic::STASH,
                "Stashes",
                snapshot.stashes.len(),
            ));
            for stash in &snapshot.stashes {
                list = list.child(stash_item(s, k, t, stash, layers));
            }
        }
        // Remote branches are sorted by name, so each remote's branches are adjacent.
        let remote_of = |name: &str| {
            name.split_once('/')
                .map_or(name.to_owned(), |(r, _)| r.to_owned())
        };
        let mut current: Option<String> = None;
        for b in &snapshot.remote {
            let remote = remote_of(&b.name);
            if current.as_deref() != Some(remote.as_str()) {
                let count = snapshot
                    .remote
                    .iter()
                    .filter(|o| remote_of(&o.name) == remote)
                    .count();
                list = list.child(section_title(k, t, &ic::CLOUD, remote.clone(), count));
                current = Some(remote);
            }
            list = list.child(branch_item(s, k, t, b, true, layers));
        }
    }

    row()
        .w(SIDEBAR_W)
        .min_w(SIDEBAR_W)
        .h_full()
        .fill(t.c(t.surface))
        .child(
            col()
                .key("sidebar-scroll")
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(list),
        )
        .child(vseparator(t))
}

#[allow(clippy::too_many_arguments)]
fn menu_trigger(
    k: &Kit,
    t: Theme,
    target: MenuTarget,
    key: String,
    menu_open: bool,
    menu_key: String,
    items: Vec<MenuItem>,
    rule_before_last: bool,
    layers: &mut Vec<El>,
) -> El {
    let ink = if menu_open {
        t.c(t.text)
    } else {
        Color::TRANSPARENT
    };
    let mut glyph = icon(&ic::MORE, 14.0).color(ink);
    if !menu_open {
        glyph = glyph.group_color(t.c(t.text));
    }
    let more = el()
        .key(format!("{key}-more"))
        .size(ROW_BUTTON, ROW_BUTTON)
        .center()
        .cursor(Cursor::Pointer)
        .hover(t.c(t.selected))
        .child(glyph)
        .on_press(move |s: &mut GitBro| {
            s.menu = Some(target.clone());
            s.recent_open = false;
        });

    let mut menu = Menu::new(menu_key)
        .trigger(more)
        .open(menu_open)
        .placement(Placement::BottomEnd)
        .gutter(2.0)
        .min_width(BRANCH_MENU_W)
        .on_close(|s: &mut GitBro| s.menu = None);
    let count = items.len();
    for (i, item) in items.into_iter().enumerate() {
        if rule_before_last && i + 1 == count {
            menu = menu.separator();
        }
        menu = menu.item(item);
    }
    menu.build(k, layers)
}

fn branch_item(
    s: &GitBro,
    k: &Kit,
    t: Theme,
    branch: &BranchInfo,
    remote: bool,
    layers: &mut Vec<El>,
) -> El {
    let target = BranchRef {
        name: branch.name.clone(),
        remote,
    };
    let selected = s.selected_branch.as_ref() == Some(&target);
    let menu_target = MenuTarget::Branch(target.clone());
    let menu_open = s.menu.as_ref() == Some(&menu_target);
    let current = branch.is_head && !remote;
    let key = format!("{}{}", if remote { "r:" } else { "l:" }, branch.name);
    // Remote branches sit under a heading with the remote's name.
    let display = if remote {
        branch
            .name
            .split_once('/')
            .map_or(branch.name.as_str(), |(_, b)| b)
            .to_owned()
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

    let mut items = Vec::new();
    let busy = s.busy();
    if !remote && current {
        items.push(
            MenuItem::new("New branch…")
                .icon(&ic::BRANCH)
                .disabled(busy)
                .on_select(|s: &mut GitBro| {
                    s.menu = None;
                    s.ask_new_branch();
                }),
        );
    } else {
        let checkout = target.clone();
        items.push(
            MenuItem::new("Check out")
                .icon(&ic::CHECKOUT)
                .disabled(busy)
                .on_select(move |s: &mut GitBro| {
                    s.menu = None;
                    let (name, remote) = (checkout.name.clone(), checkout.remote);
                    s.run("Checkout…", OpKind::Plain, move |p| {
                        git::checkout(p, &name, remote)
                    });
                }),
        );
        if let Some(head) = s.head_name().map(str::to_owned) {
            let source = target.name.clone();
            items.push(
                MenuItem::new(format!("Merge into {head}"))
                    .icon(&ic::MERGE)
                    .disabled(busy)
                    .on_select(move |s: &mut GitBro| {
                        s.menu = None;
                        let source = source.clone();
                        s.run("Merge…", OpKind::Plain, move |p| git::merge(p, &source));
                    }),
            );
        }
        // Remote branches belong to the remote; only local ones can be deleted from here.
        if !remote {
            let doomed = target.name.clone();
            items.push(
                MenuItem::new("Delete…")
                    .icon(&ic::TRASH)
                    .danger(true)
                    .disabled(busy)
                    .on_select(move |s: &mut GitBro| {
                        s.menu = None;
                        s.ask_delete_branch(doomed.clone());
                    }),
            );
        }
    }

    let clicked = branch.clone();
    let double = branch.clone();
    let main = row()
        .h(ROW_BUTTON)
        .grow(1.0)
        .min_w(0.0)
        .gap(8.0)
        .items_center()
        .pl(10.0)
        .cursor(Cursor::Pointer)
        .on_press_tx(move |s: &mut GitBro, tx: &mut Tx| {
            s.select_branch(&clicked, remote, tx);
        })
        .on_double_press(move |s: &mut GitBro| {
            if !double.is_head && !s.busy() {
                let (name, remote) = (double.name.clone(), remote);
                s.run("Checkout…", OpKind::Plain, move |p| {
                    git::checkout(p, &name, remote)
                });
            }
        })
        .child(
            text(display)
                .font(if current {
                    bold(k.ty.ui_compact)
                } else {
                    k.ty.ui_compact
                })
                .color(t.c(t.text))
                .grow(1.0)
                .truncate(),
        )
        .child(text(badge).font(tiny(k)).color(t.c(t.yellow)));

    let menu_key = format!("branch-menu-{key}");
    let trigger = menu_trigger(
        k,
        t,
        menu_target,
        key.clone(),
        menu_open,
        menu_key,
        items,
        !remote && !current,
        layers,
    );

    let mut outer = row()
        .h(ROW_BUTTON)
        .w_full()
        .items_center()
        .hover_group()
        .hover(t.c(t.border))
        .fill(if selected {
            t.c(t.selected)
        } else {
            Color::TRANSPARENT
        })
        .child(el().w(2.0).h_full().fill(if current {
            t.c(t.accent)
        } else {
            Color::TRANSPARENT
        }))
        .child(main)
        .child(trigger);
    outer = outer.key(format!("branch-{key}"));
    outer
}

fn stash_item(s: &GitBro, k: &Kit, t: Theme, stash: &StashInfo, layers: &mut Vec<El>) -> El {
    let target = MenuTarget::Stash(stash.index);
    let key = format!("s:{}", stash.index);
    // Git writes "WIP on main: abc1234 subject" or "On main: your name".
    let display = stash
        .message
        .strip_prefix("WIP on ")
        .or_else(|| stash.message.strip_prefix("On "))
        .unwrap_or(&stash.message)
        .to_owned();
    let menu_open = s.menu.as_ref() == Some(&target);
    let busy = s.busy();

    let index = stash.index;
    let message_for_drop = stash.message.clone();
    let items = vec![
        MenuItem::new("Pop")
            .icon(&ic::UNSTASH)
            .disabled(busy)
            .on_select(move |s: &mut GitBro| {
                s.menu = None;
                s.run("Popping stash…", OpKind::Plain, move |p| {
                    git::stash_pop(p, index)
                });
            }),
        MenuItem::new("Delete…")
            .icon(&ic::TRASH)
            .danger(true)
            .disabled(busy)
            .on_select(move |s: &mut GitBro| {
                s.menu = None;
                s.open_dialog(DialogKind::DropStash {
                    index,
                    message: message_for_drop.clone(),
                });
            }),
    ];
    let menu_key = format!("stash-menu-{}", stash.index);
    let trigger = menu_trigger(
        k,
        t,
        target,
        key.clone(),
        menu_open,
        menu_key,
        items,
        true,
        layers,
    );

    row()
        .key(format!("stash-row-{key}"))
        .h(ROW_BUTTON)
        .w_full()
        .items_center()
        .hover_group()
        .hover(t.c(t.border))
        .child(el().w(2.0).h_full().fill(Color::TRANSPARENT))
        .child(
            text(display)
                .font(k.ty.ui_compact)
                .color(t.c(t.text))
                .pl(10.0)
                .grow(1.0)
                .truncate(),
        )
        .child(trigger)
}

// ---------------------------------------------------------------------------
// History
// ---------------------------------------------------------------------------

fn history(s: &GitBro, cx: &mut Cx<GitBro>, k: &Kit, t: Theme) -> El {
    if let Some(dir) = &s.no_repo {
        return init_prompt(k, t, dir.clone(), s.busy());
    }
    let Some(snapshot) = &s.snapshot else {
        return welcome(s, k, t);
    };

    let (show_author, show_date) = cx
        .layout_with("history-pane", |r| (r[2] >= 880.0, r[2] >= 640.0))
        .unwrap_or((false, false));
    let graph_w = GRAPH_PAD * 2.0 + LANE_W * s.graph.width as f32;
    let count = snapshot.commits.len();
    let version = s.history_version;
    let scroll_key = s.history_scroll_key();

    let body = if count == 0 {
        centered(k, t, "No commits yet.")
    } else {
        col()
            .key(scroll_key)
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .child(
                col()
                    .key("history-rows")
                    .growth(GrowthSpec::default())
                    .virtual_rows_version(version)
                    .virtual_rows(
                        count,
                        ROW_H,
                        |s: &GitBro, ix| {
                            let oid = s
                                .snapshot
                                .as_ref()
                                .and_then(|snap| snap.commits.get(ix))
                                .map_or(Oid::ZERO_SHA1, |c| c.oid);
                            Cow::Owned(format!("commit-{oid}"))
                        },
                        move |s: &GitBro, ix| {
                            history_row(s, &s.kit, s.theme, ix, graph_w, show_author, show_date)
                        },
                    ),
            )
    };

    let mut pane = col()
        .key("history-pane")
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .child(body);
    if snapshot.truncated {
        pane = pane.child(
            row().px(12.0).py(4.0).child(
                text(format!(
                    "Showing only the newest {} commits.",
                    git::MAX_COMMITS
                ))
                .font(xs(k))
                .color(t.c(t.subtext)),
            ),
        );
    }
    pane
}

fn history_row(
    s: &GitBro,
    k: &Kit,
    t: Theme,
    ix: usize,
    graph_w: f32,
    show_author: bool,
    show_date: bool,
) -> El {
    let Some(snapshot) = &s.snapshot else {
        return el().h(ROW_H);
    };
    let Some(commit) = snapshot.commits.get(ix) else {
        return el().h(ROW_H);
    };
    let graph_row = s.graph.rows.get(ix).cloned().unwrap_or_default();
    let selected = s.selected == Some(commit.oid);
    let wip = commit.wip;

    let local_names: Vec<&str> = commit
        .refs
        .iter()
        .filter(|r| r.kind == RefKind::Local)
        .map(|r| r.name.as_str())
        .collect();
    let short_remote = |name: &str| {
        name.split_once('/')
            .map_or(name.to_owned(), |(_, b)| b.to_owned())
    };
    let mut refs: Vec<El> = Vec::new();
    for r in &commit.refs {
        // `main` and `origin/main` on the same commit read as one label with a cloud mark.
        if r.kind == RefKind::Remote && local_names.contains(&short_remote(&r.name).as_str()) {
            continue;
        }
        let synced = r.kind == RefKind::Local
            && commit
                .refs
                .iter()
                .any(|o| o.kind == RefKind::Remote && short_remote(&o.name) == r.name);
        let font = if r.is_head { bold(tiny(k)) } else { tiny(k) };
        let pill = match r.kind {
            RefKind::Local => row()
                .h(18.0)
                .px(4.0)
                .mr(4.0)
                .gap(2.0)
                .items_center()
                .fill(t.c(t.accent))
                .child(text(r.name.clone()).font(font).color(t.c(t.bg)))
                .children(synced.then(|| icon(&ic::CLOUD, 11.0).color(t.c(t.bg)))),
            RefKind::Remote => row()
                .h(18.0)
                .px(4.0)
                .mr(4.0)
                .gap(2.0)
                .items_center()
                .border(1.0, t.c(t.green))
                .child(icon(&ic::CLOUD, 11.0).color(t.c(t.green)))
                .child(text(r.name.clone()).font(font).color(t.c(t.green))),
            RefKind::Tag => row()
                .h(18.0)
                .px(4.0)
                .mr(4.0)
                .gap(2.0)
                .items_center()
                .fill(t.c(t.yellow))
                .child(icon(&ic::TAG, 11.0).color(t.c(t.bg)))
                .child(text(r.name.clone()).font(font).color(t.c(t.bg))),
            RefKind::Head => row()
                .h(18.0)
                .px(4.0)
                .mr(4.0)
                .items_center()
                .fill(t.c(t.red))
                .child(text(r.name.clone()).font(font).color(t.c(t.bg))),
        };
        refs.push(pill);
    }

    let graph = graph_shape(t, &graph_row, wip);
    let oid = commit.oid;
    let content = row()
        .h(ROW_H)
        .grow(1.0)
        .min_w(0.0)
        .items_center()
        .children(wip.then(|| {
            row()
                .h(16.0)
                .px(4.0)
                .mr(4.0)
                .center()
                .border(1.0, t.c(t.yellow))
                .child(text("WIP").font(tiny(k)).color(t.c(t.yellow)))
        }))
        .children(refs)
        .child(
            text(commit.summary.clone())
                .font(k.ty.ui_compact)
                .color(if wip { t.c(t.subtext) } else { t.c(t.text) })
                .grow(1.0)
                .truncate(),
        );

    let mut row_el = row()
        .key(format!("commit-row-{oid}"))
        .h(ROW_H)
        .w_full()
        .items_center()
        .pr(12.0)
        .cursor(Cursor::Pointer)
        .fill(if selected {
            t.c(t.selected)
        } else {
            Color::TRANSPARENT
        })
        .hover(t.c(t.border))
        .on_press_tx(move |s: &mut GitBro, tx: &mut Tx| {
            if let Some(ix) = s.commit_index(oid) {
                s.open_commit(ix, tx);
            }
        })
        .child(canvas(graph).w(graph_w).min_w(graph_w).h(ROW_H))
        .child(content);

    if show_author {
        row_el = row_el.child(
            text(commit.author.clone())
                .font(xs(k))
                .color(t.c(t.subtext))
                .w(120.0)
                .min_w(120.0)
                .pl(8.0)
                .truncate(),
        );
    }
    if show_date {
        row_el = row_el.child(
            text(commit.time.clone())
                .font(xs(k))
                .color(t.c(t.subtext))
                .w(140.0)
                .min_w(140.0)
                .pl(8.0)
                .truncate(),
        );
    }
    row_el.child(
        text(commit.short.clone())
            .font(tiny(k).mono())
            .color(t.c(t.subtext))
            .w(64.0)
            .min_w(64.0)
            .pl(8.0),
    )
}

fn graph_shape(t: Theme, row: &GraphRow, wip: bool) -> Shape {
    let mut shape = Shape::new();
    let lane_x = |lane: usize| GRAPH_PAD + LANE_W * lane as f32 + LANE_W / 2.0;
    let top = 0.0;
    let mid = ROW_H / 2.0;
    let bottom = ROW_H;
    let node_x = lane_x(row.lane);

    for &(lane, c) in &row.through {
        let x = lane_x(lane);
        shape = shape.line([x, top], [x, bottom], 2.0, LineCap::Butt, t.lane(c));
    }
    for &(lane, c) in &row.incoming {
        let x = lane_x(lane);
        if lane != row.lane {
            let path = DrawPath::new()
                .move_to([x, top])
                .quad_to([x, mid], [node_x, mid]);
            shape = shape.stroke_path(&path, 2.0, LineCap::Butt, t.lane(c));
        } else {
            shape = shape.line([x, top], [node_x, mid], 2.0, LineCap::Butt, t.lane(c));
        }
    }
    for &(lane, c) in &row.outgoing {
        let x = lane_x(lane);
        if lane != row.lane {
            let path = DrawPath::new()
                .move_to([node_x, mid])
                .quad_to([x, mid], [x, bottom]);
            shape = shape.stroke_path(&path, 2.0, LineCap::Butt, t.lane(c));
        } else {
            shape = shape.line([node_x, mid], [x, bottom], 2.0, LineCap::Butt, t.lane(c));
        }
    }

    let r = NODE_R;
    let points = [
        [node_x - r, mid - r],
        [node_x + r, mid - r],
        [node_x + r, mid + r],
        [node_x - r, mid + r],
    ];
    if wip {
        shape.stroke_with(points, true, 2.0, LineCap::Butt, t.lane(row.color))
    } else {
        shape.fill(points, t.lane(row.color))
    }
}

fn init_prompt(k: &Kit, t: Theme, dir: PathBuf, busy: bool) -> El {
    let name = dir.file_name().map_or_else(
        || dir.to_string_lossy().into_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    let full = dir.to_string_lossy().into_owned();
    let target = dir.clone();
    col()
        .grow(1.0)
        .min_w(0.0)
        .w_full()
        .center()
        .px(24.0)
        .gap(12.0)
        .child(icon(&ic::FOLDER, 44.0).color(t.c(t.selected)))
        .child(
            text(name)
                .font(TextStyle {
                    size: 20.0,
                    weight: 700.0,
                    ..k.ty.ui
                })
                .color(t.c(t.text)),
        )
        .child(
            text(full)
                .font(xs(k))
                .color(t.c(t.subtext))
                .max_w(560.0)
                .truncate(),
        )
        .child(
            text("This folder is not a Git repository yet.")
                .font(k.ty.ui)
                .color(t.c(t.subtext)),
        )
        .child(
            tool_button_accent(k, t, &ic::BRANCH, "Create Git repository", !busy, move |s| {
                s.init_repo(target.clone());
            })
            .h(32.0)
            .px(12.0)
            .border(1.0, t.c(t.accent)),
        )
        .child(tool_button_tx(
            k,
            t,
            &ic::FOLDER,
            "Open another repository",
            true,
            |s, tx| s.open_repo(tx),
        ))
}

fn welcome(s: &GitBro, k: &Kit, t: Theme) -> El {
    let mut pane = col()
        .grow(1.0)
        .w_full()
        .center()
        .px(24.0)
        .gap(12.0)
        .child(icon(&ic::BRANCH, 44.0).color(t.c(t.selected)))
        .child(
            text("GitBro")
                .font(TextStyle {
                    size: 20.0,
                    weight: 700.0,
                    ..k.ty.ui
                })
                .color(t.c(t.text)),
        )
        .child(
            text("Open a repository to see its history.")
                .font(k.ty.ui)
                .color(t.c(t.subtext)),
        )
        .child(
            tool_button_tx(k, t, &ic::FOLDER, "Open repository", true, |s, tx| {
                s.open_repo(tx)
            })
            .h(32.0)
            .px(12.0)
            .border(1.0, t.c(t.selected)),
        );
    if !s.recent.is_empty() {
        pane = pane.child(
            col()
                .mt(12.0)
                .w_full()
                .max_w(440.0)
                .border(1.0, t.c(t.border))
                .child(
                    row()
                        .px(12.0)
                        .py(4.0)
                        .child(text("RECENT").font(tiny(k)).color(t.c(t.subtext))),
                )
                .children(recent_rows(s, k, t, "welcome")),
        );
    }
    pane
}

// ---------------------------------------------------------------------------
// Files sidebar and commit panel
// ---------------------------------------------------------------------------

fn files_sidebar(s: &GitBro, k: &Kit, t: Theme) -> El {
    let Some(view) = &s.detail else {
        return row().w(FILES_SIDEBAR_W);
    };
    let body = match &view.data {
        DetailData::Ready(d) => col()
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .child(file_list(s, k, t, d, view))
            .children(d.wip.then(|| commit_panel(s, k, t, d))),
        DetailData::Loading => centered(k, t, "Loading…"),
        DetailData::Failed(_) => centered(k, t, "Failed to load"),
    };
    row()
        .w(FILES_SIDEBAR_W)
        .min_w(FILES_SIDEBAR_W)
        .h_full()
        .fill(t.c(t.surface))
        .child(
            col()
                .grow(1.0)
                .min_h(0.0)
                .w_full()
                .child(
                    row().h(40.0).px(8.0).items_center().child(
                        primary_button(k, t, &ic::ARROW_LEFT, "History", true, t.c(t.accent))
                            .on_press_tx(|s: &mut GitBro, tx: &mut Tx| s.close_detail(tx)),
                    ),
                )
                .child(body),
        )
        .child(vseparator(t))
}

fn file_list(s: &GitBro, k: &Kit, t: Theme, d: &Rc<CommitDetail>, view: &DetailView) -> El {
    let shown = d.files.len();
    let header = if d.files_total > shown {
        format!("{shown} of {} files", d.files_total)
    } else if d.files_total == 1 {
        "1 file".to_owned()
    } else {
        format!("{} files", d.files_total)
    };
    let count = d.files.len();
    let selected = view.file;
    let scope = s.files_scroll_key();

    let list = col()
        .key(scope)
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(
            col()
                .key("file-rows")
                .growth(GrowthSpec::default())
                .virtual_rows(
                    count,
                    26.0,
                    |_s: &GitBro, ix| Cow::Owned(format!("file-{ix}")),
                    move |s: &GitBro, ix| {
                        let Some(DetailView {
                            data: DetailData::Ready(d),
                            ..
                        }) = s.detail.as_ref()
                        else {
                            return el().h(26.0);
                        };
                        let Some(file) = d.files.get(ix) else {
                            return el().h(26.0);
                        };
                        file_row(s, &s.kit, s.theme, ix, file, selected == ix)
                    },
                ),
        );

    col()
        .grow(1.0)
        .min_h(0.0)
        .w_full()
        .child(separator(t))
        .child(
            row()
                .h(24.0)
                .px(8.0)
                .gap(8.0)
                .items_center()
                .child(text(header).font(xs(k)).color(t.c(t.subtext)).grow(1.0))
                .child(
                    text(format!("+{}", d.added))
                        .font(xs(k))
                        .color(t.c(t.green)),
                )
                .child(
                    text(format!("−{}", d.removed))
                        .font(xs(k))
                        .color(t.c(t.red)),
                ),
        )
        .child(separator(t))
        .child(list)
}

fn file_row(s: &GitBro, k: &Kit, t: Theme, ix: usize, file: &FileDiff, selected: bool) -> El {
    let _ = s;
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

    row()
        .key(format!("file-row-{ix}"))
        .h(26.0)
        .w_full()
        .px(8.0)
        .gap(8.0)
        .items_center()
        .cursor(Cursor::Pointer)
        .fill(if selected {
            t.c(t.selected)
        } else {
            Color::TRANSPARENT
        })
        .hover(t.c(t.border))
        .on_press(move |s: &mut GitBro| s.select_file(ix))
        .child(
            el().size(16.0, 16.0)
                .center()
                .fill(t.c(color).alpha(0.19))
                .child(
                    text(file.status.to_string())
                        .font(bold(tiny(k)))
                        .color(t.c(color)),
                ),
        )
        .child(
            row()
                .grow(1.0)
                .min_w(0.0)
                .gap(8.0)
                .items_center()
                .child(
                    text(name)
                        .font(k.ty.ui_compact)
                        .color(t.c(t.text))
                        .truncate(),
                )
                .child(
                    text(dir)
                        .font(tiny(k))
                        .color(t.c(t.subtext))
                        .grow(1.0)
                        .truncate(),
                ),
        )
        .child(
            text(format!("+{}", file.added))
                .font(tiny(k))
                .color(t.c(t.green)),
        )
        .child(
            text(format!("−{}", file.removed))
                .font(tiny(k))
                .color(t.c(t.red)),
        )
}

fn commit_panel(s: &GitBro, k: &Kit, t: Theme, d: &Rc<CommitDetail>) -> El {
    let busy = s.busy();
    let snapshot = s.snapshot.as_ref();
    let has_remote = snapshot.is_some_and(|s| s.has_remote);
    let head = snapshot.and_then(|s| s.local.iter().find(|b| b.is_head));
    // Amend needs a commit to amend, and during a merge the next commit is the merge commit.
    let can_amend = head.is_some() && !snapshot.is_some_and(|s| s.merging);
    // An amended commit that the remote already has can only be replaced there.
    let rewrites_remote = s.amend && has_remote && head.is_some_and(|b| b.upstream && b.ahead == 0);
    let has_message = !s.commit.text.trim().is_empty();
    let info = match d.files_total {
        1 => "1 file changed".to_owned(),
        n => format!("{n} files changed"),
    };
    let label_text = match (s.amend, has_remote, rewrites_remote) {
        (false, false, _) => "Commit",
        (false, true, _) => "Commit & Push",
        (true, false, _) => "Amend",
        (true, true, false) => "Amend & Push",
        (true, true, true) => "Amend & Force-Push",
    };
    let amend_on = s.amend;

    let mut content = col().padding(8.0).gap(8.0).w_full();
    if can_amend {
        content = content.child(
            row()
                .key("amend")
                .h(22.0)
                .gap(8.0)
                .items_center()
                .cursor(Cursor::Pointer)
                .on_press(move |s: &mut GitBro| s.set_amend(!amend_on))
                .child(
                    el().size(16.0, 16.0)
                        .center()
                        .border(
                            1.0,
                            if amend_on {
                                t.c(t.accent)
                            } else {
                                t.c(t.subtext)
                            },
                        )
                        .children(amend_on.then(|| el().size(8.0, 8.0).fill(t.c(t.accent)))),
                )
                .child(text("Amend last commit").font(k.ty.ui).color(t.c(t.text))),
        );
    }
    content = content
        .child(
            TextArea::new(&s.commit)
                .rows(4)
                .width(None)
                .placeholder("Commit message (first line is the title)")
                .on_edit(commit_edit)
                .on_submit(|s: &mut GitBro, _tx: &mut Tx| s.do_commit())
                .on_key(commit_field_key)
                .build(k),
        )
        .child(
            row()
                .gap(8.0)
                .items_center()
                .child(text(info).font(xs(k)).color(t.c(t.subtext)).grow(1.0))
                .child(text("Ctrl+Enter").font(xs(k)).color(t.c(t.subtext))),
        );
    if rewrites_remote {
        content = content.child(
            text("This commit is already on the remote. The push replaces it there (force-with-lease).")
                .font(xs(k))
                .color(t.c(t.yellow))
                .wrap(),
        );
    }
    content = content.child({
        let mut button = primary_button(
            k,
            t,
            &ic::COMMIT,
            label_text,
            has_message && !busy,
            if rewrites_remote {
                t.c(t.yellow)
            } else {
                t.c(t.accent)
            },
        )
        .w_full();
        if has_message && !busy {
            button = button.on_press(|s: &mut GitBro| s.do_commit());
        }
        button
    });
    content = content.child(
        row()
            .gap(8.0)
            .w_full()
            .child(
                tool_button(k, t, &ic::STASH, "Stash", !busy, |s| {
                    s.open_dialog(DialogKind::Stash)
                })
                .grow(1.0)
                .justify_center()
                .h(30.0)
                .border(1.0, t.c(t.border)),
            )
            .child(
                tool_button(k, t, &ic::TRASH, "Discard", !busy, |s| s.ask_discard())
                    .grow(1.0)
                    .justify_center()
                    .h(30.0)
                    .border(1.0, t.c(t.border)),
            ),
    );

    col().w_full().child(separator(t)).child(content)
}

// ---------------------------------------------------------------------------
// Diff view
// ---------------------------------------------------------------------------

fn diff_view(s: &GitBro, cx: &mut Cx<GitBro>, k: &Kit, t: Theme) -> El {
    let Some(view) = &s.detail else {
        return col().grow(1.0);
    };
    let pane_w = cx.layout_with("diff-pane", |r| r[2]).unwrap_or(800.0);
    let ix = s.commit_index(view.oid);
    let len = s.snapshot.as_ref().map_or(0, |s| s.commits.len());
    let title = match &view.data {
        DetailData::Ready(d) => d.summary.clone(),
        _ => "Commit".to_owned(),
    };

    let title_bar = col()
        .h(40.0)
        .w_full()
        .fill(t.c(t.surface))
        .child(
            row()
                .grow(1.0)
                .w_full()
                .pl(12.0)
                .pr(8.0)
                .gap(4.0)
                .items_center()
                .child(
                    text(title)
                        .font(bold(k.ty.ui))
                        .color(t.c(t.text))
                        .grow(1.0)
                        .truncate(),
                )
                .child(tool_button_tx(
                    k,
                    t,
                    &ic::CHEVRON_UP,
                    "Newer",
                    ix.is_some_and(|i| i > 0),
                    |s, tx| s.open_adjacent(-1, tx),
                ))
                .child(tool_button_tx(
                    k,
                    t,
                    &ic::CHEVRON_DOWN,
                    "Older",
                    ix.is_some_and(|i| i + 1 < len),
                    |s, tx| s.open_adjacent(1, tx),
                )),
        )
        .child(separator(t));

    let body = match &view.data {
        DetailData::Loading => centered(k, t, "Loading commit…"),
        DetailData::Failed(err) => col()
            .padding(12.0)
            .child(text(err.clone()).font(k.ty.ui).color(t.c(t.red))),
        DetailData::Ready(d) => col()
            .grow(1.0)
            .min_h(0.0)
            .w_full()
            .children((!d.wip).then(|| commit_meta(k, t, d)))
            .child(diff_pane(s, k, t, d, view, pane_w)),
    };

    col()
        .key("diff-pane")
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .child(title_bar)
        .child(body)
}

fn commit_meta(k: &Kit, t: Theme, d: &Rc<CommitDetail>) -> El {
    let full = d.id.clone();
    let short: String = d.id.chars().take(10).collect();
    let parents = if d.parents.is_empty() {
        "Root commit".to_owned()
    } else {
        format!("Parent: {}", d.parents.join(", "))
    };
    let mut inner = col().gap(4.0).w_full().px(12.0).py(8.0).child(
        row()
            .flex_wrap()
            .items_center()
            .gaps(4.0, 16.0)
            .child(
                text(format!("{} <{}>", d.author, d.email))
                    .font(xs(k))
                    .color(t.c(t.text)),
            )
            .child(text(d.time.clone()).font(xs(k)).color(t.c(t.subtext)))
            .child(text(parents).font(xs(k)).color(t.c(t.subtext)))
            .child(
                row()
                    .key("copy-hash")
                    .gap(6.0)
                    .items_center()
                    .cursor(Cursor::Pointer)
                    .hover(t.c(t.border))
                    .on_press_tx(move |s: &mut GitBro, tx: &mut Tx| {
                        tx.copy(&full);
                        s.set_status(Status::Done("Hash copied".into()));
                    })
                    .child(text(short).font(tiny(k).mono()).color(t.c(t.accent)))
                    .child(icon(&ic::COPY, 12.0).color(t.c(t.accent))),
            ),
    );
    if !d.body.is_empty() {
        inner = inner.child(
            col()
                .key("commit-body-scroll")
                .max_h(84.0)
                .w_full()
                .scroll(ScrollSpec {
                    stick_bottom: false,
                })
                .child(text(d.body.clone()).font(k.ty.ui).color(t.c(t.text)).wrap()),
        );
    }
    col().w_full().child(inner).child(separator(t))
}

fn diff_pane(
    s: &GitBro,
    k: &Kit,
    t: Theme,
    d: &Rc<CommitDetail>,
    view: &DetailView,
    pane_w: f32,
) -> El {
    let Some(file) = d.files.get(view.file) else {
        return centered(k, t, "No files changed");
    };

    let header = col()
        .h(24.0)
        .w_full()
        .child(
            row()
                .grow(1.0)
                .w_full()
                .px(12.0)
                .gap(8.0)
                .items_center()
                .child(
                    text(file.path.clone())
                        .font(xs(k))
                        .color(t.c(t.text))
                        .grow(1.0)
                        .truncate(),
                )
                .child(
                    text(format!("+{}", file.added))
                        .font(xs(k))
                        .color(t.c(t.green)),
                )
                .child(
                    text(format!("−{}", file.removed))
                        .font(xs(k))
                        .color(t.c(t.red)),
                ),
        )
        .child(separator(t));

    let content = if file.binary {
        centered(k, t, "Binary file")
    } else if file.lines.is_empty() {
        centered(k, t, "No text changes")
    } else {
        let widest = file
            .lines
            .iter()
            .map(|l| l.text.chars().count())
            .max()
            .unwrap_or(0);
        let content_w = (48.0 * 2.0 + 14.0 + 16.0 + widest as f32 * 13.0 * 0.6).max(pane_w);
        let count = file.lines.len();
        let scope = s.diff_scroll_key();
        col()
            .key(scope)
            .grow(1.0)
            .min_h(0.0)
            .min_w(0.0)
            .w_full()
            .scroll_xy()
            .child(
                col()
                    .key("diff-rows")
                    .growth(GrowthSpec::default())
                    .w(content_w)
                    .virtual_rows(
                        count,
                        20.0,
                        |_s: &GitBro, ix| Cow::Owned(format!("diff-line-{ix}")),
                        move |s: &GitBro, ix| diff_row(s, &s.kit, s.theme, ix, content_w),
                    ),
            )
    };

    col()
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .child(header)
        .child(content)
}

fn diff_row(s: &GitBro, k: &Kit, t: Theme, ix: usize, content_w: f32) -> El {
    let Some(DetailView {
        data: DetailData::Ready(d),
        file,
        ..
    }) = s.detail.as_ref()
    else {
        return el().h(20.0);
    };
    let Some(line) = d.files.get(*file).and_then(|f| f.lines.get(ix)) else {
        return el().h(20.0);
    };
    diff_line_row(k, t, ix, line, content_w)
}

fn diff_line_row(k: &Kit, t: Theme, ix: usize, line: &DiffLine, content_w: f32) -> El {
    let (tint, fg, sign) = match line.kind {
        LineKind::Add => (Some(t.c(t.green).alpha(0.19)), t.c(t.text), "+"),
        LineKind::Del => (Some(t.c(t.red).alpha(0.19)), t.c(t.text), "-"),
        LineKind::Hunk => (Some(t.c(t.accent).alpha(0.13)), t.c(t.accent), ""),
        LineKind::Note => (None, t.c(t.subtext), ""),
        LineKind::Context => (None, t.c(t.text), " "),
    };
    let mono = tiny(k).mono();
    let number = |n: Option<u32>| {
        text(n.map(|n| n.to_string()).unwrap_or_default())
            .font(mono)
            .color(t.c(t.subtext))
            .w(48.0)
            .min_w(48.0)
            .pr(8.0)
            .align(Align::End)
    };
    let mut row_el = row()
        .key(format!("diff-row-{ix}"))
        .h(20.0)
        .w(content_w)
        .items_center()
        .child(number(line.old))
        .child(number(line.new))
        .child(text(sign).font(mono).color(fg).w(14.0).min_w(14.0))
        .child(text(line.text.clone()).font(mono).color(fg).pr(16.0));
    if let Some(c) = tint {
        row_el = row_el.fill(c);
    }
    row_el
}

// ---------------------------------------------------------------------------
// Recent repositories
// ---------------------------------------------------------------------------

fn recent_rows(s: &GitBro, k: &Kit, t: Theme, prefix: &str) -> Vec<El> {
    s.recent
        .iter()
        .enumerate()
        .map(|(ix, path)| {
            let name = path.file_name().map_or_else(
                || path.to_string_lossy().into_owned(),
                |n| n.to_string_lossy().into_owned(),
            );
            let dir = path
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let target = path.clone();
            let remove = path.clone();
            row()
                .key(format!("{prefix}-recent-{ix}"))
                .h(32.0)
                .w_full()
                .pl(12.0)
                .pr(4.0)
                .gap(8.0)
                .items_center()
                .cursor(Cursor::Pointer)
                .hover(t.c(t.selected))
                .on_press(move |s: &mut GitBro| s.open_path(target.clone()))
                .child(icon(&ic::FOLDER, 14.0).color(t.c(t.subtext)))
                .child(text(name).font(bold(k.ty.ui_compact)).color(t.c(t.text)))
                .child(
                    text(dir)
                        .font(tiny(k))
                        .color(t.c(t.subtext))
                        .grow(1.0)
                        .truncate(),
                )
                .child(
                    el().key(format!("{prefix}-recent-x-{ix}"))
                        .size(24.0, 24.0)
                        .center()
                        .cursor(Cursor::Pointer)
                        .hover(t.c(t.border))
                        .on_press(move |s: &mut GitBro| s.forget_recent(&remove))
                        .child(icon(&ic::X, 12.0).color(t.c(t.subtext))),
                )
        })
        .collect()
}

fn recent_menu(s: &GitBro, k: &Kit, t: Theme, layers: &mut Vec<El>) {
    layers.push(
        el().key("recent-scrim")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .on_press(|s: &mut GitBro| s.close_menus()),
    );
    let mut panel = col()
        .key("recent-menu")
        .pin(Some(8.0), Some(40.0), Some(8.0), None)
        .max_w(460.0)
        .fill(t.c(t.surface))
        .border(1.0, t.c(t.selected))
        .shadow(6.0, 16.0, Color::BLACK.alpha(0.35))
        .on_press(|_: &mut GitBro| {})
        .children(recent_rows(s, k, t, "menu"))
        .child(separator(t));
    panel = panel.child(
        row()
            .h(30.0)
            .px(12.0)
            .gap(8.0)
            .items_center()
            .cursor(Cursor::Pointer)
            .hover(t.c(t.selected))
            .on_press_tx(|s: &mut GitBro, tx: &mut Tx| {
                s.recent_open = false;
                s.open_repo(tx);
            })
            .child(icon(&ic::PLUS, 14.0).color(t.c(t.text)))
            .child(
                text("Open another repository…")
                    .font(k.ty.ui_compact)
                    .color(t.c(t.text)),
            ),
    );
    layers.push(panel.modal(true, |s: &mut GitBro| s.close_menus()));
}

// ---------------------------------------------------------------------------
// Dialog
// ---------------------------------------------------------------------------

fn dialog_view(s: &GitBro, k: &Kit, t: Theme, layers: &mut Vec<El>) {
    let Some(dialog) = &s.dialog else { return };
    let (title, note, confirm, confirm_icon, danger) = match &dialog.kind {
        DialogKind::NewBranch { from } => (
            "New branch".to_owned(),
            format!("Starts at “{from}” and is checked out right away. Uncommitted changes come along."),
            "Create",
            &ic::BRANCH,
            false,
        ),
        DialogKind::Stash => (
            "Stash changes".to_owned(),
            "All changes, including new files, go into a stash. You will find it afterwards in the sidebar under “Stashes”."
                .to_owned(),
            "Stash",
            &ic::STASH,
            false,
        ),
        DialogKind::Discard { files, merging } => (
            "Discard all changes?".to_owned(),
            format!(
                "{} Everything goes back to the last commit and new files are deleted.{} This cannot be undone.",
                match files {
                    1 => "One file with changes:".to_owned(),
                    n => format!("{n} files with changes:"),
                },
                if *merging { " The open merge is aborted." } else { "" },
            ),
            "Discard",
            &ic::TRASH,
            true,
        ),
        DialogKind::DeleteBranch { name, unique } => (
            format!("Delete branch “{name}”?"),
            match unique {
                0 => "Everything in this branch is also reachable elsewhere (another branch, remote or tag). Nothing is lost.".to_owned(),
                1 => "One commit exists only in this branch and is lost when it is deleted.".to_owned(),
                n => format!("{n} commits exist only in this branch and are lost when it is deleted."),
            },
            "Delete",
            &ic::TRASH,
            true,
        ),
        DialogKind::DropStash { message, .. } => (
            "Delete stash?".to_owned(),
            format!("“{message}” is removed permanently. The changes saved in it are lost."),
            "Delete",
            &ic::TRASH,
            true,
        ),
    };
    let ready = match &dialog.kind {
        DialogKind::NewBranch { .. } => dialog
            .input
            .as_ref()
            .is_some_and(|i| !i.text.trim().is_empty()),
        _ => true,
    };
    let busy = s.busy();
    let placeholder = match &dialog.kind {
        DialogKind::NewBranch { .. } => "Branch name",
        DialogKind::Stash => "Name (optional)",
        _ => "",
    };

    let mut card = col()
        .key("dialog-card")
        .w_full()
        .max_w(440.0)
        .padding(16.0)
        .gap(12.0)
        .fill(t.c(t.surface))
        .border(1.0, t.c(t.selected))
        .shadow(8.0, 24.0, Color::BLACK.alpha(0.35))
        // Presses on the card itself must not reach the backdrop behind it.
        .on_press(|_: &mut GitBro| {})
        .child(text(title).font(bold(k.ty.ui)).color(t.c(t.text)))
        .child(text(note).font(k.ty.ui).color(t.c(t.subtext)).wrap());
    if let Some(input) = &dialog.input {
        card = card.child(
            TextInput::new(input)
                .width(None)
                .placeholder(placeholder)
                .on_edit(dialog_edit)
                .on_submit(|s: &mut GitBro, _tx: &mut Tx| s.confirm_dialog())
                .on_key(dialog_field_key)
                .build(k),
        );
    }

    let cancel = tool_button(k, t, &ic::X, "Cancel", true, |s| s.close_dialog())
        .h(30.0)
        .border(1.0, t.c(t.border));
    let mut confirm_button = primary_button(
        k,
        t,
        confirm_icon,
        confirm,
        ready && !busy,
        if danger { t.c(t.red) } else { t.c(t.accent) },
    );
    if ready && !busy {
        confirm_button = confirm_button
            .default_button()
            .on_press(|s: &mut GitBro| s.confirm_dialog());
    }
    card = card.child(
        row()
            .justify_end()
            .gap(8.0)
            .w_full()
            .child(cancel)
            .child(confirm_button),
    );

    // One full-window modal layer: the dimmed backdrop closes on a press, the card consumes
    // presses inside it, and the modal keeps the view underneath inert.
    layers.push(
        el().key("dialog-layer")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .padding(16.0)
            .center()
            .fill(Color::BLACK.alpha(0.53))
            .on_press(|s: &mut GitBro| s.close_dialog())
            .modal(false, |s: &mut GitBro| s.close_dialog())
            .child(card),
    );
}

/// The login dialog for a git or ssh prompt. Unlike `dialog_view`, a press beside the card
/// does not close it: that would cancel the operation behind it by accident.
fn login_view(s: &GitBro, k: &Kit, t: Theme, layers: &mut Vec<El>) {
    let Some(login) = &s.login else { return };
    let github = |host: &str| host == "github.com";
    let (title, note, confirm, ready) = match &login.prompt {
        Prompt::Username { host } => (
            format!("Sign in to {host}"),
            format!(
                "Git needs a login for {host} to continue. GitBro hands it to Git and keeps nothing.{}",
                if github(host) {
                    " GitHub takes a personal access token here, not your account password."
                } else {
                    ""
                }
            ),
            "Sign in",
            !login.user.text.trim().is_empty(),
        ),
        Prompt::Password { host, user } => (
            format!("Sign in to {host}"),
            format!(
                "Git needs the password{} to continue.{}",
                user.as_ref()
                    .map(|u| format!(" for “{u}”"))
                    .unwrap_or_default(),
                if github(host) {
                    " GitHub takes a personal access token here, not your account password."
                } else {
                    ""
                }
            ),
            "Sign in",
            true,
        ),
        Prompt::TrustHost { text } => (
            "Trust this server?".to_owned(),
            format!(
                "{text}\n\nOnly trust it if the fingerprint is the one your Git host publishes."
            ),
            "Trust",
            true,
        ),
        Prompt::Other { text, .. } => (
            "Git needs an answer".to_owned(),
            text.clone(),
            "Continue",
            true,
        ),
    };

    let mut card = col()
        .key("login-card")
        .w_full()
        .max_w(440.0)
        .padding(16.0)
        .gap(12.0)
        .fill(t.c(t.surface))
        .border(1.0, t.c(t.selected))
        .shadow(8.0, 24.0, Color::BLACK.alpha(0.35))
        .on_press(|_: &mut GitBro| {})
        .child(text(title).font(bold(k.ty.ui)).color(t.c(t.text)))
        .child(text(note).font(k.ty.ui).color(t.c(t.subtext)).wrap());
    match &login.prompt {
        Prompt::Username { .. } => {
            card = card
                .child(
                    TextInput::new(&login.user)
                        .width(None)
                        .placeholder("User name")
                        .on_edit(login_user)
                        .on_submit(login_user_submit)
                        .on_key(login_field_key)
                        .build(k),
                )
                .child(
                    TextInput::new(&login.answer)
                        .width(None)
                        .placeholder("Password or token")
                        .secure()
                        .on_edit(login_answer)
                        .on_submit(|s: &mut GitBro, _tx: &mut Tx| s.submit_login())
                        .on_key(login_field_key)
                        .build(k),
                );
        }
        Prompt::TrustHost { .. } => {}
        prompt => {
            let secret = match prompt {
                Prompt::Other { secret, .. } => *secret,
                _ => true,
            };
            let mut input = TextInput::new(&login.answer)
                .width(None)
                .placeholder(if secret {
                    "Password or token"
                } else {
                    "Answer"
                });
            if secret {
                input = input.secure();
            }
            card = card.child(
                input
                    .on_edit(login_answer)
                    .on_submit(|s: &mut GitBro, _tx: &mut Tx| s.submit_login())
                    .on_key(login_field_key)
                    .build(k),
            );
        }
    }

    let cancel = tool_button(k, t, &ic::X, "Cancel", true, |s| s.cancel_login())
        .h(30.0)
        .border(1.0, t.c(t.border));
    let mut confirm_button = primary_button(k, t, &ic::KEY, confirm, ready, t.c(t.accent));
    if ready {
        confirm_button = confirm_button
            .default_button()
            .on_press(|s: &mut GitBro| s.submit_login());
    }
    card = card.child(
        row()
            .justify_end()
            .gap(8.0)
            .w_full()
            .child(cancel)
            .child(confirm_button),
    );

    layers.push(
        el().key("login-layer")
            .absolute(0.0, 0.0)
            .w_full()
            .h_full()
            .padding(16.0)
            .center()
            .fill(Color::BLACK.alpha(0.53))
            .modal(false, |s: &mut GitBro| s.cancel_login())
            .child(card),
    );
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn status_text(s: &GitBro) -> (String, Color) {
    let t = s.theme;
    match &s.status {
        Status::Idle => (String::new(), t.c(t.subtext)),
        Status::Busy(msg) => (msg.clone(), t.c(t.accent)),
        Status::Done(msg) => (msg.clone(), t.c(t.green)),
        Status::Failed(msg) => (msg.clone(), t.c(t.red)),
    }
}

fn status_bar(s: &GitBro, k: &Kit, t: Theme) -> El {
    let (value, color) = status_text(s);
    let first = value
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_owned();
    let has_more = matches!(s.status, Status::Done(_) | Status::Failed(_))
        && (value.trim().lines().count() > 1 || value.chars().count() > 100);
    let open = has_more && s.status_open;

    let bar = if open {
        col()
            .key("status-scroll")
            .w_full()
            .min_h(STATUS_H)
            .max_h(240.0)
            .scroll(ScrollSpec {
                stick_bottom: false,
            })
            .on_press(|s: &mut GitBro| s.status_open = false)
            .child(
                row()
                    .w_full()
                    .px(12.0)
                    .py(8.0)
                    .gap(12.0)
                    .items_start()
                    .child(
                        text(value.trim().to_owned())
                            .font(xs(k))
                            .color(color)
                            .wrap()
                            .grow(1.0)
                            .min_w(0.0),
                    )
                    .child(text("Details ▾").font(xs(k)).color(t.c(t.subtext))),
            )
    } else {
        let mut bar = row()
            .key("status")
            .w_full()
            .h(STATUS_H)
            .px(12.0)
            .gap(12.0)
            .items_center()
            .child(
                text(first)
                    .font(xs(k))
                    .color(color)
                    .grow(1.0)
                    .min_w(0.0)
                    .truncate(),
            );
        if has_more {
            bar = bar
                .cursor(Cursor::Pointer)
                .on_press(|s: &mut GitBro| s.status_open = true)
                .child(text("Details ▴").font(xs(k)).color(t.c(t.subtext)));
        }
        bar
    };

    col()
        .w_full()
        .fill(t.c(t.surface))
        .child(separator(t))
        .child(bar)
}

// ---------------------------------------------------------------------------
// Persistence and startup
// ---------------------------------------------------------------------------

fn state_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("gitbro"))
}

/// Recently opened repositories, most recent first; folders that no longer exist are dropped.
fn load_recent() -> Vec<PathBuf> {
    let Some(dir) = state_dir() else {
        return Vec::new();
    };
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
    if let Some(arg) = rune::extra_args().iter().find(|a| !a.starts_with('-')) {
        return Some(PathBuf::from(arg));
    }
    load_recent().into_iter().next().or_else(|| {
        let cwd = std::env::current_dir().ok()?;
        git2::Repository::discover(&cwd).ok().map(|_| cwd)
    })
}

/// The monospace font Omarchy is configured with, used for diffs.
fn mono_family() -> &'static str {
    let name = std::process::Command::new("omarchy")
        .args(["font", "current"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "monospace".into());
    Box::leak(name.into_boxed_str())
}

fn main() {
    // git and ssh start this binary to ask for a login; see `askpass`.
    askpass::run_if_helper();
    rune::set_app_id("gitbro");
    rune::App::new(AppSpec::<GitBro> {
        families: ("Inter", mono_family()),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        ..AppSpec::new("GitBro", (1280.0, 800.0), init, view)
    })
    .on_focus(on_focus)
    .run();
}
