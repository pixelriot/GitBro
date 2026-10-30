use anyhow::{Context as _, Result, bail};
use git2::{
    BranchType, Delta, DiffOptions, ErrorCode, Oid, Patch, Repository, RepositoryState, Sort,
    StatusOptions,
};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Upper bound for the commits shown in the graph, keeps huge repos responsive.
pub const MAX_COMMITS: usize = 5000;
/// Limits for one commit's diff so a vendored-dependency commit can't freeze the UI.
const MAX_FILES: usize = 300;
const MAX_LINES_PER_FILE: usize = 5000;
const MAX_LINES_TOTAL: usize = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
    Head,
}

#[derive(Clone, Debug)]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
    pub is_head: bool,
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub oid: Oid,
    /// The pseudo-commit standing for uncommitted changes in the working tree.
    pub wip: bool,
    pub short: String,
    pub summary: String,
    pub author: String,
    pub time: String,
    pub parents: Vec<Oid>,
    pub refs: Vec<RefLabel>,
}

#[derive(Clone, Debug)]
pub struct BranchInfo {
    pub name: String,
    pub tip: Oid,
    pub is_head: bool,
    pub ahead: usize,
    pub behind: usize,
    /// Tracks a remote branch. Without one, `ahead` is 0 although nothing is pushed.
    pub upstream: bool,
}

#[derive(Clone, Debug)]
pub struct StashInfo {
    /// Position in the stash list: 0 is the newest, addressed as `stash@{index}`.
    pub index: usize,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub path: PathBuf,
    pub head_branch: Option<String>,
    pub commits: Vec<Commit>,
    pub local: Vec<BranchInfo>,
    pub remote: Vec<BranchInfo>,
    pub stashes: Vec<StashInfo>,
    pub merging: bool,
    /// At least one remote is configured, so a commit can be pushed.
    pub has_remote: bool,
    pub truncated: bool,
}

/// The folder exists but is not (inside) a Git repository, so the UI can offer to create one.
#[derive(Debug)]
pub struct NotARepo(pub PathBuf);

impl std::fmt::Display for NotARepo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} is not a Git repository", self.0.display())
    }
}

impl std::error::Error for NotARepo {}

pub fn load(path: &Path) -> Result<Snapshot> {
    let mut repo = match Repository::discover(path) {
        Ok(repo) => repo,
        Err(err) if err.code() == ErrorCode::NotFound && path.is_dir() => {
            return Err(NotARepo(path.to_path_buf()).into());
        }
        Err(err) => return Err(anyhow::Error::new(err).context("No Git repository found")),
    };
    // `workdir()` ends in a slash; strip it so the path compares equal to what a user picks.
    let workdir: PathBuf = repo.workdir().unwrap_or_else(|| repo.path()).components().collect();

    let mut stashes = Vec::new();
    let _ = repo.stash_foreach(|index, message, _| {
        stashes.push(StashInfo { index, message: message.to_owned() });
        true
    });

    let head = repo.head().ok();
    let head_oid = head.as_ref().and_then(|h| h.target());
    let head_branch = match &head {
        Some(h) => h
            .is_branch()
            .then(|| h.shorthand().ok().map(str::to_owned))
            .flatten(),
        None => repo.find_reference("HEAD").ok().and_then(|r| {
            r.symbolic_target()
                .ok()
                .flatten()
                .and_then(|t| t.strip_prefix("refs/heads/"))
                .map(str::to_owned)
        }),
    };

    let mut local = Vec::new();
    let mut remote = Vec::new();
    let mut labels: Vec<(Oid, RefLabel)> = Vec::new();

    for entry in repo.branches(None)? {
        let (branch, kind) = entry?;
        let Some(name) = branch.name()?.map(str::to_owned) else {
            continue;
        };
        if name.ends_with("/HEAD") {
            continue;
        }
        let Ok(commit) = branch.get().peel_to_commit() else {
            continue;
        };
        let is_head = kind == BranchType::Local && head_branch.as_deref() == Some(name.as_str());
        let upstream = kind == BranchType::Local && branch.upstream().is_ok();
        let (ahead, behind) = match (kind, branch.upstream()) {
            (BranchType::Local, Ok(up)) => up
                .get()
                .target()
                .and_then(|u| repo.graph_ahead_behind(commit.id(), u).ok())
                .unwrap_or((0, 0)),
            _ => (0, 0),
        };
        let info = BranchInfo { name: name.clone(), tip: commit.id(), is_head, ahead, behind, upstream };
        let ref_kind = match kind {
            BranchType::Local => {
                local.push(info);
                RefKind::Local
            }
            BranchType::Remote => {
                remote.push(info);
                RefKind::Remote
            }
        };
        labels.push((commit.id(), RefLabel { name, kind: ref_kind, is_head }));
    }
    local.sort_by(|a, b| b.is_head.cmp(&a.is_head).then_with(|| a.name.cmp(&b.name)));
    remote.sort_by(|a, b| a.name.cmp(&b.name));

    for name in repo.tag_names(None)?.iter().flatten().flatten() {
        if let Ok(obj) = repo.revparse_single(&format!("refs/tags/{name}"))
            && let Ok(commit) = obj.peel_to_commit()
        {
            labels.push((
                commit.id(),
                RefLabel { name: name.to_owned(), kind: RefKind::Tag, is_head: false },
            ));
        }
    }
    if head_branch.is_none()
        && let Some(oid) = head_oid
    {
        labels.push((oid, RefLabel { name: "HEAD".into(), kind: RefKind::Head, is_head: true }));
    }

    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    for glob in ["refs/heads/*", "refs/remotes/*", "refs/tags/*"] {
        // Annotated tags pointing at non-commits make this fail; nothing to show for them anyway.
        let _ = walk.push_glob(glob);
    }
    if let Some(oid) = head_oid {
        let _ = walk.push(oid);
    }

    let mut commits = Vec::new();
    let mut truncated = false;
    for oid in walk {
        if commits.len() >= MAX_COMMITS {
            truncated = true;
            break;
        }
        let commit = repo.find_commit(oid?)?;
        let id = commit.id();
        let mut refs: Vec<RefLabel> = labels
            .iter()
            .filter(|(o, _)| *o == id)
            .map(|(_, l)| l.clone())
            .collect();
        refs.sort_by_key(|l| (!l.is_head, l.kind as u8, l.name.clone()));
        let when = commit.time();
        commits.push(Commit {
            oid: id,
            wip: false,
            short: id.to_string()[..7].to_owned(),
            summary: commit.summary().ok().flatten().unwrap_or("").to_owned(),
            author: commit.author().name().unwrap_or("?").to_owned(),
            time: format_time(when.seconds() + i64::from(when.offset_minutes()) * 60),
            parents: commit.parent_ids().collect(),
            refs,
        });
    }

    let changed = working_tree_count(&repo);
    if changed > 0 {
        let at = head_oid
            .and_then(|h| commits.iter().position(|c| c.oid == h))
            .unwrap_or(0);
        commits.insert(
            at,
            Commit {
                oid: wip_oid(),
                wip: true,
                short: String::new(),
                summary: format!("Current changes ({changed})"),
                author: String::new(),
                time: String::new(),
                parents: head_oid.into_iter().collect(),
                refs: Vec::new(),
            },
        );
    }

    Ok(Snapshot {
        path: workdir,
        head_branch,
        commits,
        local,
        remote,
        stashes,
        merging: repo.state() == RepositoryState::Merge,
        has_remote: repo.remotes().is_ok_and(|r| !r.is_empty()),
        truncated,
    })
}

/// Stand-in id of the WIP pseudo-commit; no real commit can have it.
pub fn wip_oid() -> Oid {
    Oid::from_bytes(&[0xff; 20]).expect("20 bytes")
}

/// Number of files with uncommitted changes, untracked ones included.
fn working_tree_count(repo: &Repository) -> usize {
    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);
    repo.statuses(Some(&mut opts)).map_or(0, |statuses| statuses.len())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
    Hunk,
    Note,
}

#[derive(Clone, Debug)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    /// One of A(dded), D(eleted), M(odified), R(enamed), C(opied), T(ypechange).
    pub status: char,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
    pub lines: Vec<DiffLine>,
    /// Index of the widest line, used to size the horizontally scrollable diff.
    pub widest: usize,
}

#[derive(Clone, Debug)]
pub struct CommitDetail {
    /// Uncommitted changes rather than a real commit.
    pub wip: bool,
    pub id: String,
    pub summary: String,
    pub body: String,
    pub author: String,
    pub email: String,
    pub time: String,
    pub parents: Vec<String>,
    pub files: Vec<FileDiff>,
    pub files_total: usize,
    pub added: usize,
    pub removed: usize,
}

/// Reads every file of `diff` into [`FileDiff`]s. Returns them with the number of files the
/// diff really has (which may exceed what was read, see `MAX_FILES`). `budget` is the number of
/// diff lines still allowed across all files.
fn collect_files(diff: &git2::Diff<'_>, budget: &mut usize) -> Result<(Vec<FileDiff>, usize)> {
    let total = diff.deltas().len();
    let mut files = Vec::new();

    for idx in 0..total.min(MAX_FILES) {
        let Some(delta) = diff.get_delta(idx) else { continue };
        let path_of = |f: git2::DiffFile<'_>| f.path().map(|p| p.to_string_lossy().into_owned());
        let new_path = path_of(delta.new_file());
        let old_path = path_of(delta.old_file());
        let status = match delta.status() {
            Delta::Added | Delta::Untracked => 'A',
            Delta::Deleted => 'D',
            Delta::Renamed => 'R',
            Delta::Copied => 'C',
            Delta::Typechange => 'T',
            _ => 'M',
        };
        let shown = if status == 'D' { old_path.clone() } else { new_path.clone() };
        let mut file = FileDiff {
            path: shown.unwrap_or_default(),
            old_path: (matches!(status, 'R' | 'C')).then_some(old_path).flatten(),
            status,
            added: 0,
            removed: 0,
            binary: false,
            lines: Vec::new(),
            widest: 0,
        };

        // libgit2 only knows a file is binary once the patch has been generated.
        if let Some(patch) = Patch::from_diff(diff, idx)? {
            file.binary = patch.delta().flags().is_binary();
            let (_, a, r) = patch.line_stats()?;
            file.added = a;
            file.removed = r;
            if !file.binary {
                let limit = MAX_LINES_PER_FILE.min(*budget);
                let mut truncated = false;
                'hunks: for h in 0..patch.num_hunks() {
                    let (hunk, count) = patch.hunk(h)?;
                    file.lines.push(DiffLine {
                        kind: LineKind::Hunk,
                        old: None,
                        new: None,
                        text: String::from_utf8_lossy(hunk.header()).trim_end().to_owned(),
                    });
                    for l in 0..count {
                        if file.lines.len() >= limit {
                            truncated = true;
                            break 'hunks;
                        }
                        let line = patch.line_in_hunk(h, l)?;
                        let kind = match line.origin() {
                            '+' => LineKind::Add,
                            '-' => LineKind::Del,
                            ' ' => LineKind::Context,
                            _ => LineKind::Note,
                        };
                        let text = String::from_utf8_lossy(line.content())
                            .trim_end_matches(['\n', '\r'])
                            .replace('\t', "    ");
                        file.lines.push(DiffLine {
                            kind,
                            old: line.old_lineno(),
                            new: line.new_lineno(),
                            text,
                        });
                    }
                }
                if truncated {
                    file.lines.push(DiffLine {
                        kind: LineKind::Note,
                        old: None,
                        new: None,
                        text: "… diff truncated, too many lines".into(),
                    });
                }
                *budget = budget.saturating_sub(file.lines.len());
            }
        }

        file.widest = file
            .lines
            .iter()
            .enumerate()
            .max_by_key(|(_, l)| l.text.chars().count())
            .map_or(0, |(i, _)| i);
        files.push(file);
    }
    Ok((files, total))
}

/// Loads message, metadata and the diff against the first parent (the empty tree for a root commit).
/// `wip_oid()` loads the uncommitted changes instead.
pub fn load_detail(path: &Path, oid: Oid) -> Result<CommitDetail> {
    let repo = Repository::discover(path).context("No Git repository found")?;
    if oid == wip_oid() {
        return load_wip(&repo);
    }
    let commit = repo.find_commit(oid)?;
    let new_tree = commit.tree()?;
    let old_tree = commit.parent(0).ok().map(|p| p.tree()).transpose()?;

    let mut opts = DiffOptions::new();
    opts.context_lines(3);
    let mut diff = repo.diff_tree_to_tree(old_tree.as_ref(), Some(&new_tree), Some(&mut opts))?;
    diff.find_similar(None)?;

    let mut budget = MAX_LINES_TOTAL;
    let (files, files_total) = collect_files(&diff, &mut budget)?;
    let added = files.iter().map(|f| f.added).sum();
    let removed = files.iter().map(|f| f.removed).sum();

    let message = commit.message().unwrap_or("").trim().to_owned();
    let (summary, body) = match message.split_once('\n') {
        Some((s, b)) => (s.trim().to_owned(), b.trim().to_owned()),
        None => (message, String::new()),
    };
    let author = commit.author();
    let when = commit.time();
    Ok(CommitDetail {
        wip: false,
        id: oid.to_string(),
        summary,
        body,
        author: author.name().unwrap_or("?").to_owned(),
        email: author.email().unwrap_or("").to_owned(),
        time: format_time(when.seconds() + i64::from(when.offset_minutes()) * 60),
        parents: commit.parent_ids().map(|p| p.to_string()[..7].to_owned()).collect(),
        files,
        files_total,
        added,
        removed,
    })
}

/// Everything that differs from HEAD in the working tree, untracked files included. Staging is
/// not a concept the UI exposes, so index and working tree are not told apart.
fn load_wip(repo: &Repository) -> Result<CommitDetail> {
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());

    let mut opts = DiffOptions::new();
    opts.context_lines(3)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    let mut diff = repo.diff_tree_to_workdir_with_index(head_tree.as_ref(), Some(&mut opts))?;
    diff.find_similar(None)?;

    let mut budget = MAX_LINES_TOTAL;
    let (files, files_total) = collect_files(&diff, &mut budget)?;

    Ok(CommitDetail {
        wip: true,
        id: String::new(),
        summary: "Current changes".into(),
        body: String::new(),
        author: String::new(),
        email: String::new(),
        time: String::new(),
        parents: Vec::new(),
        added: files.iter().map(|f| f.added).sum(),
        removed: files.iter().map(|f| f.removed).sum(),
        files,
        files_total,
    })
}

/// Runs `git` in `path` and returns its combined output. Never prompts on the terminal
/// or opens an editor, so a missing credential fails instead of hanging.
fn run(path: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .stdin(Stdio::null())
        .output()
        .context("could not start git")?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let text = text.trim().to_owned();
    if !out.status.success() {
        let name = args.iter().find(|a| !a.starts_with('-')).unwrap_or(&args[0]);
        bail!("{}", if text.is_empty() { format!("git {name} fehlgeschlagen") } else { text });
    }
    Ok(text)
}

/// Fetches all remotes and fast-forwards the current branch to its upstream.
///
/// Diverged branches are left alone: merging them is a decision the user makes explicitly,
/// and a surprise merge commit or conflict behind a single button would be worse than a hint.
pub fn sync(path: &Path) -> Result<String> {
    run(path, &["fetch", "--all", "--prune"])?;
    if run(path, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_err() {
        return Ok("Fetch done. The branch has no upstream, so there is nothing to pull.".into());
    }
    let counts = run(path, &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])?;
    let mut parts = counts.split_whitespace().map(|n| n.parse::<usize>().unwrap_or(0));
    let (ahead, behind) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    match (ahead, behind) {
        (0, 0) => Ok("Already up to date.".into()),
        (ahead, 0) => Ok(format!("Up to date. {ahead} local commits are not pushed yet.")),
        (0, behind) => {
            run(path, &["merge", "--ff-only", "@{upstream}"])?;
            Ok(format!("Pulled {behind} new commits."))
        }
        (ahead, behind) => bail!(
            "Local (↑{ahead}) and remote (↓{behind}) have diverged, so nothing was merged.\n\
             To merge: click the remote branch in the sidebar and choose “Merge into …”."
        ),
    }
}

/// `git init`, on the branch name from the user's `init.defaultBranch` (or `main`).
pub fn init(path: &Path) -> Result<String> {
    let branch = run(path, &["config", "--get", "init.defaultBranch"])
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "main".to_owned());
    run(path, &["init", "-b", &branch])
}

/// Name of the remote to push to: `origin` if there is one, else the first.
fn remote_name(path: &Path) -> Option<String> {
    let remotes = run(path, &["remote"]).ok()?;
    let mut names = remotes.lines().map(str::trim).filter(|n| !n.is_empty());
    let first = names.next()?;
    Some(if first == "origin" || names.any(|n| n == "origin") { "origin" } else { first }.to_owned())
}

/// Pushes the current branch, setting its upstream on the first push. `force` is for a rewritten
/// commit (amend) and uses `--force-with-lease`, which refuses to overwrite commits that
/// someone else pushed in the meantime. A repository without a remote has nothing to push to.
pub fn push(path: &Path, force: bool) -> Result<String> {
    let Some(remote) = remote_name(path) else {
        return Ok("No remote configured, so nothing was pushed.".into());
    };
    let lease = force.then_some("--force-with-lease");
    let mut args = vec!["push"];
    args.extend(lease);
    if run(path, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_err() {
        args.extend(["--set-upstream", remote.as_str(), "HEAD"]);
    }
    run(path, &args)?;
    Ok(format!("Pushed to {remote}."))
}

pub fn merge(path: &Path, branch: &str) -> Result<String> {
    run(path, &["merge", "--no-edit", branch])
}

pub fn abort_merge(path: &Path) -> Result<String> {
    run(path, &["merge", "--abort"])
}

/// `git switch` creates a tracking branch when given a remote branch's short name.
pub fn checkout(path: &Path, branch: &str, remote: bool) -> Result<String> {
    if remote {
        let local = branch.split_once('/').map_or(branch, |(_, b)| b);
        run(path, &["switch", local])
    } else {
        run(path, &["switch", branch])
    }
}

/// Commits every change in the working tree, new files included, or with `amend` folds them into
/// the last commit under the new message. During a merge this concludes the merge.
pub fn commit(path: &Path, message: &str, amend: bool) -> Result<String> {
    let message = message.trim();
    if message.is_empty() {
        bail!("The commit message is empty.");
    }
    // `add -A` would silently mark conflict markers as resolved.
    let conflicted = run(path, &["diff", "--name-only", "--diff-filter=U"])?;
    if !conflicted.is_empty() {
        bail!("There are still unresolved merge conflicts:\n{conflicted}");
    }
    run(path, &["add", "-A"])?;
    if amend {
        run(path, &["commit", "--amend", "-m", message])
    } else {
        run(path, &["commit", "-m", message])
    }
}

/// The full message of the last commit, to start an amend from.
pub fn head_message(path: &Path) -> Result<String> {
    let repo = Repository::discover(path).context("No Git repository found")?;
    let commit = repo.head()?.peel_to_commit()?;
    Ok(commit.message().unwrap_or("").trim().to_owned())
}

/// Throws away all uncommitted changes: edits, staged changes, untracked files and an unfinished
/// merge. Ignored files stay. This cannot be undone.
pub fn discard_all(path: &Path) -> Result<String> {
    if run(path, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok() {
        run(path, &["reset", "--hard", "-q"])?;
    } else {
        // Before the first commit there is no HEAD to reset to, only an index to empty.
        run(path, &["read-tree", "--empty"])?;
    }
    run(path, &["clean", "-fdq"])?;
    Ok("Changes discarded.".into())
}

/// Puts the uncommitted changes, untracked files included, on the stash.
pub fn stash_push(path: &Path, message: &str) -> Result<String> {
    let message = message.trim();
    if message.is_empty() {
        run(path, &["stash", "push", "--include-untracked"])
    } else {
        run(path, &["stash", "push", "--include-untracked", "-m", message])
    }
}

pub fn stash_apply(path: &Path, index: usize) -> Result<String> {
    run(path, &["stash", "apply", &format!("stash@{{{index}}}")])
}

pub fn stash_pop(path: &Path, index: usize) -> Result<String> {
    run(path, &["stash", "pop", &format!("stash@{{{index}}}")])
}

pub fn stash_drop(path: &Path, index: usize) -> Result<String> {
    run(path, &["stash", "drop", &format!("stash@{{{index}}}")])
}

/// Creates a branch at HEAD and switches to it; uncommitted changes come along.
pub fn create_branch(path: &Path, name: &str) -> Result<String> {
    run(path, &["switch", "-c", name])
}

/// Deletes a local branch even if it is not merged; callers warn about that beforehand.
pub fn delete_branch(path: &Path, name: &str) -> Result<String> {
    run(path, &["branch", "-D", name])
}

/// Number of commits that exist only on this local branch, i.e. that no other branch, remote
/// branch, tag or HEAD can reach. Those are the ones deleting the branch would orphan.
pub fn unique_commits(path: &Path, name: &str) -> Result<usize> {
    let repo = Repository::discover(path).context("No Git repository found")?;
    let own = format!("refs/heads/{name}");
    let tip = repo.find_reference(&own)?.peel_to_commit()?.id();
    let mut walk = repo.revwalk()?;
    walk.push(tip)?;
    if let Some(head) = repo.head().ok().and_then(|h| h.target()) {
        walk.hide(head)?;
    }
    for reference in repo.references()? {
        let reference = reference?;
        let Ok(refname) = reference.name() else { continue };
        let relevant = ["refs/heads/", "refs/remotes/", "refs/tags/"].iter().any(|p| refname.starts_with(p));
        if !relevant || refname == own {
            continue;
        }
        if let Ok(commit) = reference.peel_to_commit() {
            walk.hide(commit.id())?;
        }
    }
    Ok(walk.count())
}

fn format_time(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sh(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=T", "-c", "user.email=t@t"])
            .args(args)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    fn commit(dir: &Path, file: &str, content: &str) {
        fs::write(dir.join(file), content).unwrap();
        sh(dir, &["add", "."]);
        sh(dir, &["commit", "-q", "-m", &format!("edit {file}")]);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gitbro-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn repo(dir: &Path) {
        sh(dir, &["init", "-q", "-b", "main"]);
        // The functions under test run plain `git`, so they need an identity of their own.
        sh(dir, &["config", "user.name", "T"]);
        sh(dir, &["config", "user.email", "t@t"]);
        commit(dir, "a.txt", "1\n");
    }

    #[test]
    fn merge_checkout_and_snapshot() {
        let dir = scratch("merge");
        repo(&dir);
        sh(&dir, &["switch", "-q", "-c", "topic"]);
        commit(&dir, "b.txt", "b\n");
        checkout(&dir, "main", false).unwrap();
        commit(&dir, "c.txt", "c\n");
        merge(&dir, "topic").unwrap();

        let snap = load(&dir).unwrap();
        assert_eq!(snap.head_branch.as_deref(), Some("main"));
        assert!(!snap.merging);
        let tip = &snap.commits[0];
        assert_eq!(tip.parents.len(), 2, "merge commit expected");
        assert_eq!(snap.local.len(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn conflict_can_be_aborted() {
        let dir = scratch("conflict");
        repo(&dir);
        sh(&dir, &["switch", "-q", "-c", "topic"]);
        commit(&dir, "a.txt", "topic\n");
        checkout(&dir, "main", false).unwrap();
        commit(&dir, "a.txt", "main\n");

        let err = merge(&dir, "topic").unwrap_err().to_string();
        assert!(err.contains("CONFLICT"), "{err}");
        assert!(load(&dir).unwrap().merging);
        abort_merge(&dir).unwrap();
        assert!(!load(&dir).unwrap().merging);
        fs::remove_dir_all(dir).unwrap();
    }

    /// Bare remote `origin.git` plus two clones, `a` (with one commit pushed) and `b`.
    fn remote_pair(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = scratch(name);
        let bare = root.join("origin.git");
        let (a, b) = (root.join("a"), root.join("b"));
        fs::create_dir_all(&bare).unwrap();
        sh(&bare, &["init", "-q", "--bare", "-b", "main"]);
        fs::create_dir_all(&a).unwrap();
        repo(&a);
        sh(&a, &["remote", "add", "origin", bare.to_str().unwrap()]);
        push(&a, false).unwrap(); // no upstream yet: sets it
        sh(&root, &["clone", "-q", bare.to_str().unwrap(), "b"]);
        (root, a, b)
    }

    #[test]
    fn push_sets_upstream_and_sync_fast_forwards() {
        let (root, a, b) = remote_pair("sync-ff");
        commit(&a, "a.txt", "2\n");
        push(&a, false).unwrap();

        assert_eq!(load(&b).unwrap().commits.len(), 1);
        let msg = sync(&b).unwrap();
        assert!(msg.contains("Pulled 1 new commits"), "{msg}");
        let snap = load(&b).unwrap();
        assert_eq!(snap.commits.len(), 2);
        assert_eq!(snap.local[0].behind, 0);
        assert!(sync(&b).unwrap().contains("up to date"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sync_does_not_merge_diverged_branches() {
        let (root, a, b) = remote_pair("sync-diverged");
        commit(&a, "a.txt", "remote\n");
        push(&a, false).unwrap();
        commit(&b, "b.txt", "local\n");

        let err = sync(&b).unwrap_err().to_string();
        assert!(err.contains("diverged"), "{err}");
        let snap = load(&b).unwrap();
        assert!(!snap.merging);
        assert_eq!(snap.commits.iter().filter(|c| c.parents.len() == 2).count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sync_without_upstream_only_fetches() {
        let dir = scratch("sync-none");
        repo(&dir);
        let msg = sync(&dir).unwrap();
        assert!(msg.contains("no upstream"), "{msg}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn detail_lists_files_and_lines() {
        let dir = scratch("detail");
        repo(&dir);
        fs::write(dir.join("a.txt"), "1\ntwo\n").unwrap();
        fs::write(dir.join("new.txt"), "new\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "Title\n\nLong text"]);

        let snap = load(&dir).unwrap();
        let d = load_detail(&dir, snap.commits[0].oid).unwrap();
        assert_eq!((d.summary.as_str(), d.body.as_str()), ("Title", "Long text"));
        assert_eq!(d.files.len(), 2);
        let a = d.files.iter().find(|f| f.path == "a.txt").unwrap();
        assert_eq!((a.status, a.added, a.removed), ('M', 1, 0));
        assert!(a.lines.iter().any(|l| l.kind == LineKind::Add && l.text == "two"));
        let n = d.files.iter().find(|f| f.path == "new.txt").unwrap();
        assert_eq!(n.status, 'A');

        fs::write(dir.join("blob.bin"), [0u8, 159, 146, 150, 0, 1, 2]).unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-q", "-m", "binary"]);
        let snap = load(&dir).unwrap();
        let bin = load_detail(&dir, snap.commits[0].oid).unwrap();
        assert!(bin.files[0].binary, "NUL bytes should be detected as binary");
        assert!(bin.files[0].lines.is_empty());

        // Root commit diffs against the empty tree.
        let root = load_detail(&dir, snap.commits[2].oid).unwrap();
        assert_eq!(root.files.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plain_folder_can_be_initialised() {
        let dir = scratch("init");
        let err = load(&dir).unwrap_err();
        assert!(err.downcast_ref::<NotARepo>().is_some(), "{err:#}");

        let missing = dir.join("nope");
        let err = load(&missing).unwrap_err();
        assert!(err.downcast_ref::<NotARepo>().is_none(), "a missing folder is a plain error");

        init(&dir).unwrap();
        let snap = load(&dir).unwrap();
        assert!(snap.commits.is_empty());
        assert!(snap.head_branch.is_some(), "an unborn branch still has a name");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn working_tree_changes_show_up_as_wip() {
        let dir = scratch("wip");
        repo(&dir);
        let clean = load(&dir).unwrap();
        assert!(clean.commits.iter().all(|c| !c.wip), "no WIP row while the tree is clean");

        fs::write(dir.join("a.txt"), "1\nchanged\n").unwrap(); // unstaged edit
        fs::write(dir.join("staged.txt"), "s\n").unwrap();
        sh(&dir, &["add", "staged.txt"]); // staged new file
        fs::write(dir.join("untracked.txt"), "u1\nu2\n").unwrap(); // untracked

        let snap = load(&dir).unwrap();
        let wip = snap.commits.iter().find(|c| c.wip).expect("WIP row");
        assert_eq!(wip.oid, wip_oid());
        assert!(wip.summary.contains("(3)"), "{}", wip.summary);
        let head = snap.commits.iter().find(|c| !c.wip).unwrap();
        assert_eq!(wip.parents, vec![head.oid], "WIP hangs off the HEAD commit");

        let d = load_detail(&dir, wip_oid()).unwrap();
        assert!(d.wip);
        let paths: Vec<_> = d.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths.len(), 3, "staged and unstaged changes share one list: {paths:?}");
        for expected in ["a.txt", "staged.txt", "untracked.txt"] {
            assert!(paths.contains(&expected), "{paths:?}");
        }
        let untracked = d.files.iter().find(|f| f.path == "untracked.txt").unwrap();
        assert_eq!((untracked.status, untracked.added), ('A', 2), "untracked content is shown as added lines");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn wip_in_a_repository_without_commits() {
        let dir = scratch("wip-unborn");
        init(&dir).unwrap();
        fs::write(dir.join("new.txt"), "hi\n").unwrap();
        let snap = load(&dir).unwrap();
        assert_eq!(snap.commits.len(), 1);
        assert!(snap.commits[0].wip && snap.commits[0].parents.is_empty());
        let d = load_detail(&dir, wip_oid()).unwrap();
        assert_eq!(d.files.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commit_takes_everything_and_amend_rewrites_the_last_commit() {
        let dir = scratch("commit");
        repo(&dir);
        fs::write(dir.join("new file *.txt"), "n\n").unwrap(); // spaces and a glob character
        fs::remove_file(dir.join("a.txt")).unwrap();

        assert!(super::commit(&dir, "  \n", false).is_err(), "an empty message is refused");
        super::commit(&dir, "Everything at once\n\nWith body", false).unwrap();
        let snap = load(&dir).unwrap();
        assert!(snap.commits.iter().all(|c| !c.wip), "nothing is left behind");
        assert_eq!(snap.commits.len(), 2);
        let d = load_detail(&dir, snap.commits[0].oid).unwrap();
        assert_eq!((d.summary.as_str(), d.body.as_str()), ("Everything at once", "With body"));
        let paths: Vec<_> = d.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "new file *.txt"], "the deletion and the new file are both in");

        assert_eq!(head_message(&dir).unwrap(), "Everything at once\n\nWith body");
        fs::write(dir.join("later.txt"), "l\n").unwrap();
        super::commit(&dir, "New text", true).unwrap();
        let snap = load(&dir).unwrap();
        assert_eq!(snap.commits.len(), 2, "amend adds no commit");
        let d = load_detail(&dir, snap.commits[0].oid).unwrap();
        assert_eq!(d.summary, "New text");
        assert_eq!(d.files.len(), 3, "the new file joined the amended commit");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commit_works_before_the_first_commit() {
        let dir = scratch("commit-unborn");
        init(&dir).unwrap();
        sh(&dir, &["config", "user.name", "T"]);
        sh(&dir, &["config", "user.email", "t@t"]);
        fs::write(dir.join("x.txt"), "x\n").unwrap();
        super::commit(&dir, "First", false).unwrap();
        assert_eq!(load(&dir).unwrap().commits.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commit_refuses_unresolved_conflicts() {
        let dir = scratch("commit-conflict");
        repo(&dir);
        sh(&dir, &["switch", "-q", "-c", "topic"]);
        commit(&dir, "a.txt", "topic\n");
        checkout(&dir, "main", false).unwrap();
        commit(&dir, "a.txt", "main\n");
        assert!(merge(&dir, "topic").is_err());
        let err = super::commit(&dir, "Too early", false).unwrap_err().to_string();
        assert!(err.contains("conflict") && err.contains("a.txt"), "{err}");
        assert!(load(&dir).unwrap().merging, "the merge is still open");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn push_after_amend_needs_force_and_without_remote_does_nothing() {
        let (root, a, b) = remote_pair("push-amend");
        commit(&a, "a.txt", "2\n");
        push(&a, false).unwrap();
        assert!(load(&a).unwrap().local[0].upstream);

        super::commit(&a, "Reworded", true).unwrap();
        assert!(push(&a, false).is_err(), "a rewritten, already pushed commit is rejected");
        push(&a, true).unwrap();
        sh(&b, &["fetch", "-q"]);
        let remote_tip = Command::new("git").arg("-C").arg(&b).args(["rev-parse", "origin/main"]).output().unwrap();
        let local_tip = Command::new("git").arg("-C").arg(&a).args(["rev-parse", "HEAD"]).output().unwrap();
        assert_eq!(remote_tip.stdout, local_tip.stdout, "the remote has the amended commit");

        let lonely = scratch("push-none");
        repo(&lonely);
        assert!(push(&lonely, false).unwrap().contains("No remote"));
        fs::remove_dir_all(lonely).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discard_removes_every_uncommitted_change() {
        let dir = scratch("discard");
        repo(&dir);
        commit(&dir, ".gitignore", "ignored.log\n");
        fs::write(dir.join("ignored.log"), "log\n").unwrap();
        fs::write(dir.join("a.txt"), "1\nedited\n").unwrap();
        fs::write(dir.join("added.txt"), "n\n").unwrap();
        sh(&dir, &["add", "added.txt"]); // staged
        fs::create_dir_all(dir.join("folder")).unwrap();
        fs::write(dir.join("folder/deep.txt"), "t\n").unwrap(); // untracked, in a new folder

        discard_all(&dir).unwrap();
        let snap = load(&dir).unwrap();
        assert!(snap.commits.iter().all(|c| !c.wip), "the tree is clean");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "1\n");
        assert!(!dir.join("added.txt").exists() && !dir.join("folder").exists());
        assert!(dir.join("ignored.log").exists(), "ignored files are not touched");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn discard_works_before_the_first_commit_and_ends_a_merge() {
        let dir = scratch("discard-unborn");
        init(&dir).unwrap();
        fs::write(dir.join("x.txt"), "x\n").unwrap();
        sh(&dir, &["add", "x.txt"]);
        fs::write(dir.join("y.txt"), "y\n").unwrap();
        discard_all(&dir).unwrap();
        assert!(load(&dir).unwrap().commits.is_empty());
        assert!(!dir.join("x.txt").exists() && !dir.join("y.txt").exists());
        fs::remove_dir_all(dir).unwrap();

        let dir = scratch("discard-merge");
        repo(&dir);
        sh(&dir, &["switch", "-q", "-c", "topic"]);
        commit(&dir, "a.txt", "topic\n");
        checkout(&dir, "main", false).unwrap();
        commit(&dir, "a.txt", "main\n");
        assert!(merge(&dir, "topic").is_err());
        discard_all(&dir).unwrap();
        let snap = load(&dir).unwrap();
        assert!(!snap.merging && snap.commits.iter().all(|c| !c.wip));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stash_roundtrip() {
        let dir = scratch("stash");
        repo(&dir);
        assert!(load(&dir).unwrap().stashes.is_empty());

        fs::write(dir.join("a.txt"), "1\nlocal\n").unwrap();
        fs::write(dir.join("added.txt"), "n\n").unwrap();
        stash_push(&dir, "My state").unwrap();
        let snap = load(&dir).unwrap();
        assert!(snap.commits.iter().all(|c| !c.wip), "the tree is clean after stashing");
        assert!(!dir.join("added.txt").exists(), "untracked files are stashed too");
        assert_eq!(snap.stashes.len(), 1);
        assert!(snap.stashes[0].message.contains("My state"), "{}", snap.stashes[0].message);

        stash_apply(&dir, 0).unwrap();
        assert_eq!(load(&dir).unwrap().stashes.len(), 1, "apply keeps the stash");
        stash_push(&dir, "").unwrap();
        assert_eq!(load(&dir).unwrap().stashes.len(), 2);
        stash_pop(&dir, 0).unwrap();
        assert_eq!(load(&dir).unwrap().stashes.len(), 1, "pop removes it");
        assert!(dir.join("added.txt").exists());
        sh(&dir, &["reset", "-q", "--hard"]);
        sh(&dir, &["clean", "-fdq"]);
        stash_drop(&dir, 0).unwrap();
        assert!(load(&dir).unwrap().stashes.is_empty());
        stash_push(&dir, "").unwrap(); // nothing to stash is no error for git
        assert!(load(&dir).unwrap().stashes.is_empty(), "and it creates no empty stash");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn branches_can_be_created_and_deleted() {
        let dir = scratch("branches");
        repo(&dir);
        commit(&dir, "b.txt", "b\n");

        create_branch(&dir, "feature").unwrap();
        let snap = load(&dir).unwrap();
        assert_eq!(snap.head_branch.as_deref(), Some("feature"));
        let tip = snap.commits.iter().find(|c| !c.wip).unwrap().oid;
        assert_eq!(snap.local.iter().find(|b| b.name == "feature").unwrap().tip, tip);
        assert!(create_branch(&dir, "feature").is_err(), "the name is taken");
        assert!(create_branch(&dir, "no good").is_err(), "not a valid branch name");

        commit(&dir, "f.txt", "f\n");
        checkout(&dir, "main", false).unwrap();
        assert_eq!(unique_commits(&dir, "feature").unwrap(), 1);
        merge(&dir, "feature").unwrap();
        assert_eq!(unique_commits(&dir, "feature").unwrap(), 0, "merged branches lose nothing");
        delete_branch(&dir, "feature").unwrap();
        assert!(load(&dir).unwrap().local.iter().all(|b| b.name != "feature"));
        fs::remove_dir_all(dir).unwrap();
    }
}
