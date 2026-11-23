//! New Pull Request, after JetBrains' GitHub "New Pull Request" tab: pick
//! `base ← head`, see the commits and files the pull request would carry
//! (all of them or one commit, with each file's diff), write the title and
//! description (or have the background model write them), choose reviewers,
//! assignees and labels, then push if needed and create it.
//!
//! Every base or head change bumps a generation; results for an older one are
//! dropped. A metadata failure after the pull request exists keeps it, and
//! Retry only sets the metadata again. What the user typed survives Cancel for
//! the same head branch ([`PrDraft`], kept by the Changes panel).
mod flow;
mod render;
#[cfg(test)]
mod tests;

use flow::{
    CheckRequest, Load, Outcome, Stage, Submission, can_create, effective_title, push_plan,
    range_context, suggested_title,
};
pub(crate) use flow::{Gh, PrBackend};

use crate::{
    change_tree::{Node, TreeRow},
    changes::TextHarness,
    dialog_resize::{self, DialogResize},
    diff_view::{DiffFile, DiffView},
    input::{self, Input},
    popover::{Menu, MenuAction, MenuEvent, MenuItem, MenuStyle},
    pr_metadata::{BuildItems, CandidateState, PrMetadataEvent, PrMetadataPicker, SearchMenu},
    theme::*,
    update_dialog::model,
};
use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, PromptLevel, SharedString,
    Subscription, Task, Window, prelude::*,
};
use monocode_core::{
    diff,
    scm::{
        self, ChangedFile, CreatedPr, Pr, PrBranchCheck, PrMetadata, PrPush, PrRange, PrRepo,
        PrSetup, PrSubmit,
    },
};
use std::{
    collections::HashSet,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// The size the user last dragged this dialog to; the next one opens that big.
static REMEMBERED_SIZE: dialog_resize::Remembered = dialog_resize::remembered();
/// Branch rows a picker lists before asking for a narrower search.
const BRANCH_ROWS: usize = 200;

// One event at a time, read and dropped at once: boxing the large
// variant would cost more than its size does.
#[allow(clippy::large_enum_variant)]
pub(crate) enum PrDialogEvent {
    /// Closed without creating: what was typed, to start from next time.
    Close(PrDraft),
    /// The pull request exists (its metadata may have failed).
    Created(CreatedPr),
    /// "View" on the pull request that already exists for these branches.
    ViewExisting(Pr),
}

/// What the user typed and picked, kept across Cancel for one head branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PrDraft {
    /// The local branch the draft belongs to.
    pub(crate) head_branch: String,
    /// Base repository slug and branch.
    pub(crate) base: Option<(String, String)>,
    pub(crate) head_repo: Option<String>,
    pub(crate) remote_branch: Option<String>,
    pub(crate) title: String,
    pub(crate) title_touched: bool,
    /// The description, once one was set (from a template or typed).
    pub(crate) body: Option<String>,
    pub(crate) template: Option<usize>,
    /// The description as the dialog last wrote it: a different one was typed.
    pub(crate) body_auto: String,
    pub(crate) metadata: PrMetadata,
}

/// Which commits the file tree and diff show.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scope {
    All,
    Commit(usize),
}

pub(crate) enum SubmitState {
    Idle,
    Running(Stage),
    Failed(String),
    MetadataFailed {
        pr: CreatedPr,
        error: String,
        submission: Box<Submission>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PopupKind {
    Base,
    Head,
    Commits,
    CreateMenu,
    Templates,
}

enum PopupMenu {
    Search(Entity<SearchMenu>),
    Plain(Entity<Menu>),
}
struct Popup {
    kind: PopupKind,
    menu: PopupMenu,
    _events: Subscription,
}

pub(crate) struct PrDialog {
    cwd: PathBuf,
    backend: Arc<dyn PrBackend>,
    harness: Option<TextHarness>,
    draft: Option<PrDraft>,
    pub(crate) setup: Load<PrSetup>,
    pub(crate) base_repo: usize,
    pub(crate) base_branch: String,
    pub(crate) head_repo: usize,
    pub(crate) head_branch: String,
    /// Bumped by every base or head change: older range results are dropped.
    pub(crate) generation: u64,
    /// Bumped by every push-state and branch check.
    check_generation: u64,
    pub(crate) range: Load<PrRange>,
    pub(crate) push: Load<PrPush>,
    pub(crate) check: Load<PrBranchCheck>,
    /// The remote branch name field shows: the branch is not on the remote.
    naming_remote: bool,
    pub(crate) scope: Scope,
    files: Load<Vec<ChangedFile>>,
    files_request: u64,
    rows: Vec<TreeRow>,
    collapsed: HashSet<String>,
    selected: Option<String>,
    diff_view: Entity<DiffView>,
    diff_request: u64,
    pub(crate) title: Entity<Input>,
    /// The title the dialog last suggested; typing anything else stops it.
    auto_title: String,
    pub(crate) title_touched: bool,
    pub(crate) body: Entity<Input>,
    template: Option<usize>,
    body_auto: String,
    body_set: bool,
    remote_branch: Entity<Input>,
    pub(crate) metadata: Entity<PrMetadataPicker>,
    candidates_request: u64,
    branch_list: Option<(usize, Load<Vec<String>>)>,
    local_branches: Load<Vec<String>>,
    popup_repo: usize,
    generate_cancel: Option<Arc<AtomicBool>>,
    generate_error: Option<String>,
    pub(crate) submit: SubmitState,
    popup: Option<Popup>,
    anchors: render::Anchors,
    focus: FocusHandle,
    size: DialogResize,
    recheck_task: Option<Task<()>>,
    /// The remote branch name the last check was for.
    checked_remote: String,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<PrDialogEvent> for PrDialog {}
impl dialog_resize::Resizable for PrDialog {
    fn dialog_resize(&mut self) -> &mut DialogResize {
        &mut self.size
    }
}
impl Focusable for PrDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl PrDialog {
    pub(crate) fn new(
        cwd: String,
        backend: Arc<dyn PrBackend>,
        draft: Option<PrDraft>,
        harness: Option<TextHarness>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let diff_view = cx.new(DiffView::new);
        diff_view.update(cx, |view, cx| {
            view.allow_split = true;
            view.source_root = Some(cwd.clone());
            view.set_split(true, cx);
        });
        let title = cx.new(|cx| Input::field("Title", "Title", content(0.35), cx));
        let body = cx.new(|cx| {
            let mut input = Input::markdown_editor("Description", cx);
            input.set_placeholder("Description (Markdown)", cx);
            input
        });
        let remote_branch =
            cx.new(|cx| Input::field("Remote branch name", "Branch name", content(0.35), cx));
        let metadata = cx.new(|_| {
            PrMetadataPicker::new(
                String::new(),
                draft
                    .as_ref()
                    .map(|d| d.metadata.clone())
                    .unwrap_or_default(),
            )
        });
        let subscriptions = vec![
            cx.subscribe(
                &diff_view,
                |this, _, event: &crate::diff_view::DiffEvent, cx| {
                    if matches!(event, crate::diff_view::DiffEvent::Jumped) && !this.busy() {
                        this.close(cx);
                    }
                },
            ),
            cx.subscribe(&title, |this, _, _: &input::Changed, cx| {
                this.title_changed(cx)
            }),
            cx.subscribe(&body, |_, _, _: &input::Changed, cx| cx.notify()),
            cx.subscribe(&remote_branch, |this, _, _: &input::Changed, cx| {
                this.schedule_recheck(cx)
            }),
            cx.subscribe(
                &metadata,
                |this, _, event: &PrMetadataEvent, cx| match event {
                    PrMetadataEvent::Changed(_) => cx.notify(),
                    PrMetadataEvent::Retry => this.load_candidates(cx),
                },
            ),
        ];
        window.focus(&title.focus_handle(cx), cx);
        let mut this = Self {
            cwd: PathBuf::from(cwd),
            backend,
            harness,
            draft,
            setup: Load::Loading,
            base_repo: 0,
            base_branch: String::new(),
            head_repo: 0,
            head_branch: String::new(),
            generation: 0,
            check_generation: 0,
            range: Load::Loading,
            push: Load::Loading,
            check: Load::Loading,
            naming_remote: false,
            scope: Scope::All,
            files: Load::Loading,
            files_request: 0,
            rows: Vec::new(),
            collapsed: HashSet::new(),
            selected: None,
            diff_view,
            diff_request: 0,
            title,
            auto_title: String::new(),
            title_touched: false,
            body,
            template: None,
            body_auto: String::new(),
            body_set: false,
            remote_branch,
            metadata,
            candidates_request: 0,
            branch_list: None,
            local_branches: Load::Loading,
            popup_repo: 0,
            generate_cancel: None,
            generate_error: None,
            submit: SubmitState::Idle,
            popup: None,
            anchors: Default::default(),
            focus: cx.focus_handle(),
            size: DialogResize::new(&REMEMBERED_SIZE).with_usual_min_height(560.),
            recheck_task: None,
            checked_remote: String::new(),
            _subscriptions: subscriptions,
        };
        this.load_setup(cx);
        this
    }

    // ---- State -------------------------------------------------------------

    pub(crate) fn busy(&self) -> bool {
        matches!(self.submit, SubmitState::Running(_))
    }
    pub(crate) fn generating(&self) -> bool {
        self.generate_cancel.is_some()
    }
    fn repo(&self, ix: usize) -> Option<&PrRepo> {
        self.setup.ready().and_then(|s| s.repos.get(ix))
    }
    pub(crate) fn base(&self) -> Option<&PrRepo> {
        self.repo(self.base_repo)
    }
    pub(crate) fn head(&self) -> Option<&PrRepo> {
        self.repo(self.head_repo)
    }
    /// The branch name on the head repository.
    pub(crate) fn remote_branch_name(&self, cx: &gpui::App) -> String {
        let typed = self.remote_branch.read(cx).content.trim().to_owned();
        if typed.is_empty() {
            self.head_branch.clone()
        } else {
            typed
        }
    }
    pub(crate) fn effective_title(&self, cx: &gpui::App) -> String {
        effective_title(&self.title.read(cx).content, &self.head_branch)
    }
    pub(crate) fn can_create(&self, cx: &gpui::App) -> bool {
        can_create(
            &self.check,
            &self.push,
            self.busy() || self.generating(),
            &self.effective_title(cx),
        )
    }
    /// What the commits of the current scope are.
    fn scope_commits(&self) -> Vec<scm::IncomingCommit> {
        let Some(range) = self.range.ready() else {
            return Vec::new();
        };
        match self.scope {
            Scope::All => range.commits.clone(),
            Scope::Commit(ix) => range.commits.get(ix).cloned().into_iter().collect(),
        }
    }
    /// The two revisions the scope's diff runs between.
    fn diff_endpoints(&self) -> Option<(String, String)> {
        let range = self.range.ready()?;
        match self.scope {
            Scope::All => Some((range.merge_base.clone(), range.head_sha.clone())),
            Scope::Commit(ix) => range
                .commits
                .get(ix)
                .map(|c| (format!("{}^", c.sha), c.sha.clone())),
        }
    }
    pub(crate) fn draft(&self, cx: &gpui::App) -> PrDraft {
        let body = self.body.read(cx).content.to_string();
        PrDraft {
            head_branch: self.head_branch.clone(),
            base: self
                .base()
                .map(|repo| (repo.slug(), self.base_branch.clone())),
            head_repo: self.head().map(PrRepo::slug),
            remote_branch: self
                .naming_remote
                .then(|| self.remote_branch.read(cx).content.trim().to_owned())
                .filter(|name| !name.is_empty()),
            title: self.title.read(cx).content.to_string(),
            title_touched: self.title_touched,
            body: (self.body_set || !body.is_empty()).then_some(body),
            template: self.template,
            body_auto: self.body_auto.clone(),
            metadata: self.metadata.read(cx).metadata().clone(),
        }
    }

    /// Closes: a pull request created before a metadata failure still counts.
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        if self.busy() {
            return;
        }
        self.cancel_generate(cx);
        if let SubmitState::MetadataFailed { pr, .. } = &self.submit {
            cx.emit(PrDialogEvent::Created(pr.clone()));
        } else {
            cx.emit(PrDialogEvent::Close(self.draft(cx)));
        }
    }

    // ---- Title and description ---------------------------------------------

    fn title_changed(&mut self, cx: &mut Context<Self>) {
        if self.title.read(cx).content.as_ref() != self.auto_title {
            self.title_touched = true;
        }
        cx.notify();
    }
    /// Suggests a title for the range until the user writes their own.
    fn suggest_title(&mut self, cx: &mut Context<Self>) {
        if self.title_touched {
            return;
        }
        let commits = self.range.ready().map(|r| r.commits.as_slice());
        let title = suggested_title(commits, &self.head_branch);
        if self.title.read(cx).content.as_ref() != title {
            self.auto_title = title.clone();
            self.title.update(cx, |input, cx| input.set_text(title, cx));
        }
    }
    /// Applies template `ix` to the description; asks first when the user
    /// already changed it.
    pub(crate) fn choose_template(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(template) = self
            .setup
            .ready()
            .and_then(|s| s.templates.get(ix))
            .cloned()
        else {
            return;
        };
        let current = self.body.read(cx).content.to_string();
        let untouched = current == self.body_auto || current.trim().is_empty();
        if untouched {
            self.apply_template(ix, template.body, cx);
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!(
                "Replace the description with the “{}” template?",
                template.name
            ),
            Some("What you wrote can be brought back with Undo."),
            &[
                gpui::PromptButton::ok("Replace"),
                gpui::PromptButton::cancel("Cancel"),
            ],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if matches!(answer.await, Ok(0)) {
                let _ = this.update(cx, |this, cx| this.apply_template(ix, template.body, cx));
            }
        })
        .detach();
    }
    fn apply_template(&mut self, ix: usize, body: String, cx: &mut Context<Self>) {
        self.template = Some(ix);
        self.body_auto = body.clone();
        self.body_set = true;
        self.body
            .update(cx, |input, cx| input.replace_all(body, cx));
        cx.notify();
    }

    // ---- Loading -----------------------------------------------------------

    pub(crate) fn load_setup(&mut self, cx: &mut Context<Self>) {
        self.setup = Load::Loading;
        cx.notify();
        let (backend, cwd) = (self.backend.clone(), self.cwd.clone());
        let work = cx
            .background_executor()
            .spawn(async move { backend.setup(&cwd) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| this.apply_setup(result, cx));
        })
        .detach();
    }

    /// Takes the loaded setup: the draft's branches when it has them, the
    /// suggested ones otherwise; then fetches and loads the range.
    pub(crate) fn apply_setup(&mut self, result: Result<PrSetup, String>, cx: &mut Context<Self>) {
        let setup = match result {
            Ok(setup) => setup,
            Err(error) => {
                self.setup = Load::Failed(error);
                cx.notify();
                return;
            }
        };
        let draft = self
            .draft
            .take()
            .filter(|d| d.head_branch == setup.head_branch);
        let find = |slug: &str| setup.repos.iter().position(|r| r.slug() == slug);
        self.base_repo = setup.base_repo.min(setup.repos.len().saturating_sub(1));
        self.base_branch = setup.base_branch.clone();
        self.head_repo = setup.head_repo.min(setup.repos.len().saturating_sub(1));
        self.head_branch = setup.head_branch.clone();
        if let Some(draft) = &draft {
            if let Some((slug, branch)) = &draft.base
                && let Some(ix) = find(slug)
            {
                self.base_repo = ix;
                self.base_branch = branch.clone();
            }
            if let Some(ix) = draft.head_repo.as_deref().and_then(find)
                && !setup.repos[ix].remote.is_empty()
            {
                self.head_repo = ix;
            }
        }
        let remote_name = draft
            .as_ref()
            .and_then(|d| d.remote_branch.clone())
            .unwrap_or_else(|| self.head_branch.clone());
        self.naming_remote = draft.as_ref().is_some_and(|d| d.remote_branch.is_some());
        self.remote_branch
            .update(cx, |input, cx| input.set_text(remote_name, cx));
        let viewer = setup.viewer.clone();
        let first_template = setup.templates.first().map(|t| t.body.clone());
        self.setup = Load::Ready(setup);
        self.metadata
            .update(cx, |picker, cx| picker.set_viewer(viewer, cx));
        match draft {
            Some(draft) => {
                self.title_touched = draft.title_touched;
                self.auto_title = if draft.title_touched {
                    String::new()
                } else {
                    draft.title.clone()
                };
                self.title
                    .update(cx, |input, cx| input.set_text(draft.title.clone(), cx));
                self.template = draft.template;
                self.body_auto = draft.body_auto.clone();
                if let Some(body) = draft.body {
                    self.body_set = true;
                    self.body.update(cx, |input, cx| input.set_text(body, cx));
                }
            }
            None => {
                if let Some(body) = first_template {
                    self.template = Some(0);
                    self.body_auto = body.clone();
                    self.body_set = true;
                    self.body.update(cx, |input, cx| input.set_text(body, cx));
                }
            }
        }
        self.suggest_title(cx);
        self.load_candidates(cx);
        self.fetch_then_reload(cx);
    }

    /// Fetches the base and head remotes quietly, then loads the range. A
    /// failed fetch (offline) still shows what the local refs know.
    fn fetch_then_reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.checked_remote = self.remote_branch_name(cx);
        self.range = Load::Loading;
        self.check = Load::Loading;
        self.push = Load::Loading;
        let generation = self.generation;
        let mut remotes: Vec<String> = [self.base(), self.head()]
            .into_iter()
            .flatten()
            .map(|r| r.remote.clone())
            .filter(|r| !r.is_empty())
            .collect();
        remotes.dedup();
        let (backend, cwd) = (self.backend.clone(), self.cwd.clone());
        let work = cx
            .background_executor()
            .spawn(async move { backend.fetch(&cwd, &remotes) });
        cx.spawn(async move |this, cx| {
            let _ = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.reload(cx);
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Base or head changed: everything about the range starts over.
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.checked_remote = self.remote_branch_name(cx);
        let generation = self.generation;
        self.range = Load::Loading;
        self.check = Load::Loading;
        self.push = Load::Loading;
        self.scope = Scope::All;
        self.files = Load::Loading;
        self.rows.clear();
        self.selected = None;
        self.diff_view
            .update(cx, |view, cx| view.set_files(vec![], cx));
        let (backend, cwd) = (self.backend.clone(), self.cwd.clone());
        let (base, base_branch) = (
            self.base().cloned().unwrap_or_default(),
            self.base_branch.clone(),
        );
        let head_ref = self.head_branch.clone();
        let work = cx.background_executor().spawn(async move {
            let base_ref = backend.base_ref(&cwd, &base, &base_branch)?;
            backend.range(&cwd, &base_ref, &head_ref)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| this.apply_range(generation, result, cx));
        })
        .detach();
        cx.notify();
    }

    /// A range result: dropped unless it is for the current base and head.
    pub(crate) fn apply_range(
        &mut self,
        generation: u64,
        result: Result<PrRange, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        match result {
            Ok(range) => {
                self.files = Load::Ready(range.files.clone());
                self.range = Load::Ready(range);
                self.suggest_title(cx);
                self.files_loaded(cx);
                self.run_check(cx);
            }
            Err(error) => {
                self.files = Load::Failed(error.clone());
                self.check = Load::Failed(error.clone());
                self.push = Load::Failed(error.clone());
                self.range = Load::Failed(error);
            }
        }
        cx.notify();
    }

    /// The remote branch name changed: check again once typing pauses.
    fn schedule_recheck(&mut self, cx: &mut Context<Self>) {
        // The dialog filling the field in, or typing back to the checked name.
        if self.remote_branch_name(cx) == self.checked_remote {
            self.recheck_task = None;
            return;
        }
        self.recheck_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let _ = this.update(cx, |this, cx| this.recheck(cx));
        }));
    }
    pub(crate) fn recheck(&mut self, cx: &mut Context<Self>) {
        if self.range.ready().is_some() {
            self.run_check(cx);
        }
    }
    fn check_request(&self, cx: &gpui::App) -> Option<CheckRequest> {
        Some(CheckRequest {
            base: self.base()?.clone(),
            base_branch: self.base_branch.clone(),
            head: self.head()?.clone(),
            head_branch: self.remote_branch_name(cx),
        })
    }
    /// What pushing would take, and whether these branches can make a pull
    /// request.
    fn run_check(&mut self, cx: &mut Context<Self>) {
        let (Some(range), Some(request)) = (self.range.ready().cloned(), self.check_request(cx))
        else {
            return;
        };
        self.check_generation += 1;
        self.checked_remote = request.head_branch.clone();
        let generation = self.check_generation;
        self.check = Load::Loading;
        self.push = Load::Loading;
        let (backend, cwd) = (self.backend.clone(), self.cwd.clone());
        let local = self.head_branch.clone();
        let work = cx.background_executor().spawn(async move {
            let push = backend.push_state(&cwd, &local, &request.head.remote, &request.head_branch);
            let check = backend.check(&cwd, &request, &range);
            (push, check)
        });
        cx.spawn(async move |this, cx| {
            let (push, check) = work.await;
            let _ = this.update(cx, |this, cx| this.apply_check(generation, push, check, cx));
        })
        .detach();
        cx.notify();
    }
    pub(crate) fn apply_check(
        &mut self,
        generation: u64,
        push: Result<PrPush, String>,
        check: Result<PrBranchCheck, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.check_generation {
            return;
        }
        if matches!(push, Ok(PrPush::Create { .. })) {
            self.naming_remote = true;
        }
        self.push = Load::from(push);
        self.check = Load::from(check);
        cx.notify();
    }

    pub(crate) fn load_candidates(&mut self, cx: &mut Context<Self>) {
        let Some(repo) = self.base().cloned() else {
            return;
        };
        self.candidates_request += 1;
        let request = self.candidates_request;
        self.metadata.update(cx, |picker, cx| {
            picker.set_candidates(CandidateState::Loading, cx)
        });
        let (backend, cwd) = (self.backend.clone(), self.cwd.clone());
        let work = cx
            .background_executor()
            .spawn(async move { backend.candidates(&cwd, &repo) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.candidates_request != request {
                    return;
                }
                let state = match result {
                    Ok(candidates) => CandidateState::Ready(candidates),
                    Err(error) => CandidateState::Failed(error),
                };
                this.metadata
                    .update(cx, |picker, cx| picker.set_candidates(state, cx));
            });
        })
        .detach();
    }

    // ---- Base and head -----------------------------------------------------

    pub(crate) fn set_base(&mut self, repo: usize, branch: String, cx: &mut Context<Self>) {
        if repo == self.base_repo && branch == self.base_branch {
            return;
        }
        let repo_changed = repo != self.base_repo;
        self.base_repo = repo;
        self.base_branch = branch;
        if repo_changed {
            self.load_candidates(cx);
            self.fetch_then_reload(cx);
        } else {
            self.reload(cx);
        }
    }
    pub(crate) fn set_head(&mut self, repo: usize, branch: String, cx: &mut Context<Self>) {
        if repo == self.head_repo && branch == self.head_branch {
            return;
        }
        let branch_changed = branch != self.head_branch;
        let repo_changed = repo != self.head_repo;
        self.head_repo = repo;
        self.head_branch = branch.clone();
        if branch_changed {
            self.naming_remote = false;
            self.remote_branch
                .update(cx, |input, cx| input.set_text(branch, cx));
        }
        if repo_changed {
            self.fetch_then_reload(cx);
        } else {
            self.reload(cx);
        }
    }

    fn load_branches(&mut self, repo: usize, cx: &mut Context<Self>) {
        if self
            .branch_list
            .as_ref()
            .is_some_and(|(ix, list)| *ix == repo && !matches!(list, Load::Failed(_)))
        {
            return;
        }
        let Some(target) = self.repo(repo).cloned() else {
            return;
        };
        self.branch_list = Some((repo, Load::Loading));
        let (backend, cwd) = (self.backend.clone(), self.cwd.clone());
        let work = cx
            .background_executor()
            .spawn(async move { backend.branches(&cwd, &target) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.branch_list.as_ref().is_some_and(|(ix, _)| *ix == repo) {
                    this.branch_list = Some((repo, Load::from(result)));
                    this.refresh_popup(cx);
                }
            });
        })
        .detach();
    }
    fn load_local_branches(&mut self, cx: &mut Context<Self>) {
        if matches!(self.local_branches, Load::Ready(_)) {
            return;
        }
        self.local_branches = Load::Loading;
        let (backend, cwd) = (self.backend.clone(), self.cwd.clone());
        let work = cx
            .background_executor()
            .spawn(async move { backend.local_branches(&cwd) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.local_branches = Load::from(result);
                this.refresh_popup(cx);
            });
        })
        .detach();
    }

    // ---- Files and diff ----------------------------------------------------

    pub(crate) fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        let Some(range) = self.range.ready() else {
            return;
        };
        if scope == self.scope {
            return;
        }
        self.scope = scope;
        self.files_request += 1;
        self.selected = None;
        self.rows.clear();
        match scope {
            Scope::All => {
                self.files = Load::Ready(range.files.clone());
                self.files_loaded(cx);
            }
            Scope::Commit(ix) => {
                let Some(sha) = range.commits.get(ix).map(|c| c.sha.clone()) else {
                    return;
                };
                self.files = Load::Loading;
                self.diff_view
                    .update(cx, |view, cx| view.set_files(vec![], cx));
                let request = self.files_request;
                let generation = self.generation;
                let (backend, cwd) = (self.backend.clone(), self.cwd.clone());
                let work = cx
                    .background_executor()
                    .spawn(async move { backend.commit_files(&cwd, &sha) });
                cx.spawn(async move |this, cx| {
                    let result = work.await;
                    let _ = this.update(cx, |this, cx| {
                        if this.files_request != request || this.generation != generation {
                            return;
                        }
                        this.files = Load::from(result);
                        this.files_loaded(cx);
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }
    pub(crate) fn file_list(&self) -> &[ChangedFile] {
        self.files.ready().map_or(&[], Vec::as_slice)
    }
    fn files_loaded(&mut self, cx: &mut Context<Self>) {
        self.rebuild_rows();
        self.selected = self.first_file();
        self.load_diff(cx);
        cx.notify();
    }
    /// The first file in tree order.
    fn first_file(&self) -> Option<String> {
        self.rows.iter().find_map(|row| match row {
            TreeRow::File { ix, .. } => self.file_list().get(*ix).map(|f| f.relative.clone()),
            _ => None,
        })
    }
    fn rebuild_rows(&mut self) {
        let files = self.file_list().to_vec();
        let mut root = Node::default();
        for (ix, file) in files.iter().enumerate() {
            root.insert(&file.relative.split('/').collect::<Vec<_>>(), ix);
        }
        let mut rows = Vec::new();
        root.flatten(&files, "pr", "", 0, &self.collapsed, &mut rows);
        self.rows = rows;
    }
    fn toggle_dir(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key);
        }
        self.rebuild_rows();
        cx.notify();
    }
    fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(path) = self.file_list().get(ix).map(|f| f.relative.clone()) else {
            return;
        };
        if self.selected.as_ref() == Some(&path) {
            return;
        }
        self.selected = Some(path);
        self.load_diff(cx);
        cx.notify();
    }
    fn load_diff(&mut self, cx: &mut Context<Self>) {
        self.diff_request += 1;
        let (Some(relative), Some((old, new))) = (self.selected.clone(), self.diff_endpoints())
        else {
            self.diff_view
                .update(cx, |view, cx| view.set_files(vec![], cx));
            return;
        };
        let request = self.diff_request;
        let id: SharedString = format!("pr-{relative}").into();
        self.diff_view.update(cx, |view, cx| {
            view.set_files(vec![model(&id, &relative, None)], cx)
        });
        let (backend, cwd, path) = (self.backend.clone(), self.cwd.clone(), relative.clone());
        let work = cx.background_executor().spawn(async move {
            backend.file_diff(&cwd, &old, &new, &path).map(|file| {
                let unified = (!file.binary && !file.too_large).then(|| {
                    diff::build_unified_file(&file.original, &file.current, diff::CONTEXT_DEFAULT)
                });
                (file, unified)
            })
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if this.diff_request != request {
                    return;
                }
                let file = match result {
                    Ok(loaded) => model(&id, &relative, Some(loaded)),
                    Err(error) => DiffFile {
                        empty_message: Some(format!("Couldn’t load diff: {error}")),
                        ..model(&id, &relative, None)
                    },
                };
                this.diff_view.update(cx, |view, cx| {
                    view.set_files(vec![file], cx);
                    view.reveal_file(id.clone(), cx);
                });
                cx.notify();
            });
        })
        .detach();
    }

    // ---- Generate ----------------------------------------------------------

    /// A title and description for the chosen commits, written by a
    /// read-only turn of the background text model. Undoable.
    pub(crate) fn generate(&mut self, cx: &mut Context<Self>) {
        if self.generating() || self.busy() {
            return;
        }
        let (Some((old, new)), Some(base)) = (self.diff_endpoints(), self.base()) else {
            return;
        };
        let Some(harness) = self.harness.clone() else {
            self.generate_error =
                Some("No provider CLI is installed to write a pull request.".into());
            cx.notify();
            return;
        };
        let base = format!("{}:{}", base.slug(), self.base_branch);
        let head = self.head_branch.clone();
        let commits = self.scope_commits();
        let files = self.file_list().to_vec();
        let cancel = Arc::new(AtomicBool::new(false));
        self.generate_cancel = Some(cancel.clone());
        self.generate_error = None;
        cx.notify();
        let cwd = self.cwd.clone();
        let flag = cancel.clone();
        let work = cx.background_executor().spawn(async move {
            let context = range_context(&cwd, &base, &head, &commits, &files, &old, &new)?;
            let kind = harness.0;
            let output =
                crate::changes::text_prompt(harness, cwd, scm::pr_content_prompt(&context), flag)?;
            scm::parse_pr_content(&output).ok_or_else(|| {
                format!(
                    "Could not generate a pull request. {} returned no usable title.",
                    kind.label()
                )
            })
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                this.generate_cancel = None;
                match result {
                    Ok((title, body)) => {
                        this.title_touched = true;
                        this.title.update(cx, |m, cx| m.replace_all(title, cx));
                        if !body.is_empty() {
                            this.body_set = true;
                            this.body.update(cx, |m, cx| m.replace_all(body, cx));
                        }
                    }
                    Err(error) => this.generate_error = Some(error),
                }
                cx.notify();
            });
        })
        .detach();
    }
    pub(crate) fn cancel_generate(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = self.generate_cancel.take() {
            cancel.store(true, Ordering::SeqCst);
            cx.notify();
        }
    }

    // ---- Create ------------------------------------------------------------

    /// Push if needed, create, then set the metadata.
    pub(crate) fn create(&mut self, draft: bool, cx: &mut Context<Self>) {
        if !self.can_create(cx) {
            return;
        }
        let (Some(base), Some(head), Some(push)) = (
            self.base().cloned(),
            self.head().cloned(),
            self.push.ready(),
        ) else {
            return;
        };
        let submission = Submission {
            cwd: self.cwd.clone(),
            push: push_plan(&self.head_branch, push),
            submit: PrSubmit {
                base,
                base_branch: self.base_branch.clone(),
                head,
                head_branch: self.remote_branch_name(cx),
                title: self.effective_title(cx),
                body: self.body.read(cx).content.to_string(),
                draft,
            },
            metadata: self.metadata.read(cx).metadata().clone(),
            created: None,
        };
        self.popup = None;
        self.run_submission(submission, cx);
    }
    /// After "created, but setting metadata failed": sets the metadata again,
    /// never creating a second pull request.
    pub(crate) fn retry_metadata(&mut self, cx: &mut Context<Self>) {
        let SubmitState::MetadataFailed { submission, .. } =
            std::mem::replace(&mut self.submit, SubmitState::Idle)
        else {
            return;
        };
        let mut submission = *submission;
        // Whatever the picker holds now, on the pull request that exists.
        submission.metadata = self.metadata.read(cx).metadata().clone();
        self.run_submission(submission, cx);
    }
    fn run_submission(&mut self, submission: Submission, cx: &mut Context<Self>) {
        let stages = submission.stages();
        let Some(&first) = stages.first() else {
            self.finish(submission.outcome(None), submission, cx);
            return;
        };
        self.submit = SubmitState::Running(first);
        self.metadata
            .update(cx, |picker, cx| picker.set_enabled(false, cx));
        cx.notify();
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let mut submission = submission;
            let mut failed = None;
            for stage in stages {
                let _ = this.update(cx, |this, cx| {
                    this.submit = SubmitState::Running(stage);
                    cx.notify();
                });
                let backend = backend.clone();
                let (back, result) = cx
                    .background_executor()
                    .spawn(async move {
                        let result = submission.run(stage, &*backend);
                        (submission, result)
                    })
                    .await;
                submission = back;
                if let Err(error) = result {
                    failed = Some((stage, error));
                    break;
                }
            }
            let outcome = submission.outcome(failed);
            let _ = this.update(cx, |this, cx| this.finish(outcome, submission, cx));
        })
        .detach();
    }
    fn finish(&mut self, outcome: Outcome, submission: Submission, cx: &mut Context<Self>) {
        self.metadata
            .update(cx, |picker, cx| picker.set_enabled(true, cx));
        match outcome {
            Outcome::Created(pr) => {
                self.submit = SubmitState::Idle;
                cx.emit(PrDialogEvent::Created(pr));
            }
            Outcome::MetadataFailed(pr, error) => {
                self.submit = SubmitState::MetadataFailed {
                    pr,
                    error,
                    submission: Box::new(submission),
                };
            }
            Outcome::Failed(error) => {
                self.submit = SubmitState::Failed(error);
                // A push may have gone through before the failure.
                if submission.push.is_some() {
                    self.recheck(cx);
                }
            }
        }
        cx.notify();
    }

    // ---- Popups ------------------------------------------------------------

    pub(crate) fn toggle_popup(
        &mut self,
        kind: PopupKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.popup.take().is_some_and(|p| p.kind == kind) || self.busy() {
            cx.notify();
            return;
        }
        let menu = match kind {
            PopupKind::Base | PopupKind::Head => {
                self.popup_repo = if kind == PopupKind::Base {
                    self.base_repo
                } else {
                    self.head_repo
                };
                if kind == PopupKind::Base {
                    self.load_branches(self.popup_repo, cx);
                } else {
                    self.load_local_branches(cx);
                }
                let build = self.popup_build(kind);
                let (label, placeholder) = if kind == PopupKind::Base {
                    ("Base branch", "Search branches…")
                } else {
                    ("Head branch", "Search local branches…")
                };
                PopupMenu::Search(
                    cx.new(|cx| SearchMenu::new(label, placeholder, 300., build, window, cx)),
                )
            }
            PopupKind::Commits | PopupKind::CreateMenu | PopupKind::Templates => {
                let items = self.plain_items(kind);
                if items.is_empty() {
                    return;
                }
                let label = match kind {
                    PopupKind::Commits => "Commits",
                    PopupKind::CreateMenu => "Create options",
                    _ => "Templates",
                };
                PopupMenu::Plain(cx.new(|cx| {
                    Menu::new(label, items, window, cx)
                        .width(if kind == PopupKind::Commits {
                            380.
                        } else {
                            240.
                        })
                        .style(MenuStyle::Git)
                }))
            }
        };
        let events = match &menu {
            PopupMenu::Search(menu) => cx.subscribe_in(
                menu,
                window,
                move |this, _, event: &MenuEvent, window, cx| {
                    this.popup_event(kind, event, window, cx)
                },
            ),
            PopupMenu::Plain(menu) => cx.subscribe_in(
                menu,
                window,
                move |this, _, event: &MenuEvent, window, cx| {
                    this.popup_event(kind, event, window, cx)
                },
            ),
        };
        self.popup = Some(Popup {
            kind,
            menu,
            _events: events,
        });
        cx.notify();
    }
    fn refresh_popup(&mut self, cx: &mut Context<Self>) {
        let Some(popup) = &self.popup else {
            return;
        };
        if let PopupMenu::Search(menu) = &popup.menu {
            let build = self.popup_build(popup.kind);
            menu.update(cx, |menu, cx| menu.set_build(build, cx));
        }
    }
    fn popup_event(
        &mut self,
        kind: PopupKind,
        event: &MenuEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = match event {
            MenuEvent::Dismissed => {
                self.popup = None;
                cx.notify();
                return;
            }
            MenuEvent::Picked(id, _) => id.to_string(),
        };
        if let Some(repo) = id
            .strip_prefix("repo:")
            .and_then(|n| n.parse::<usize>().ok())
        {
            self.popup_repo = repo;
            match kind {
                PopupKind::Base => self.load_branches(repo, cx),
                PopupKind::Head => {
                    let branch = self.head_branch.clone();
                    self.set_head(repo, branch, cx);
                }
                _ => {}
            }
            self.refresh_popup(cx);
            cx.notify();
            return;
        }
        if id == "retry-branches" {
            match kind {
                PopupKind::Base => {
                    self.branch_list = None;
                    self.load_branches(self.popup_repo, cx);
                }
                _ => self.load_local_branches(cx),
            }
            self.refresh_popup(cx);
            return;
        }
        self.popup = None;
        match (kind, id.split_once(':')) {
            (PopupKind::Base, Some(("branch", name))) => {
                self.set_base(self.popup_repo, name.to_owned(), cx)
            }
            (PopupKind::Head, Some(("branch", name))) => {
                self.set_head(self.head_repo, name.to_owned(), cx)
            }
            (PopupKind::Commits, Some(("commit", n))) => {
                if let Ok(ix) = n.parse() {
                    self.set_scope(Scope::Commit(ix), cx)
                }
            }
            (PopupKind::Commits, _) if id == "all" => self.set_scope(Scope::All, cx),
            (PopupKind::CreateMenu, _) if id == "draft" => self.create(true, cx),
            (PopupKind::Templates, Some(("template", n))) => {
                if let Ok(ix) = n.parse() {
                    self.choose_template(ix, window, cx)
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// The rows of the base or head picker, for the current data.
    fn popup_build(&self, kind: PopupKind) -> BuildItems {
        let repos: Vec<(usize, String)> = self
            .setup
            .ready()
            .map(|s| {
                s.repos
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| kind == PopupKind::Base || !r.remote.is_empty())
                    .map(|(ix, r)| (ix, r.slug()))
                    .collect()
            })
            .unwrap_or_default();
        let popup_repo = self.popup_repo;
        let (branches, chosen) = if kind == PopupKind::Base {
            let list = match &self.branch_list {
                Some((ix, list)) if *ix == popup_repo => list.clone(),
                _ => Load::Loading,
            };
            let chosen = (popup_repo == self.base_repo).then(|| self.base_branch.clone());
            (list, chosen)
        } else {
            (self.local_branches.clone(), Some(self.head_branch.clone()))
        };
        Rc::new(move |query: &str| {
            let mut items = Vec::new();
            if repos.len() > 1 {
                items.push(MenuItem::Header("Repository".into()));
                for (ix, slug) in &repos {
                    items.push(
                        MenuAction::new(format!("repo:{ix}"), slug.clone())
                            .icon("git-branch")
                            .checked(*ix == popup_repo)
                            .keep_open()
                            .into(),
                    );
                }
                items.push(MenuItem::Header("Branches".into()));
            }
            match &branches {
                Load::Loading => items.push(MenuItem::Note("Loading branches…".into())),
                Load::Failed(error) => {
                    items.push(MenuItem::Note(
                        format!("Couldn’t load branches: {error}").into(),
                    ));
                    items.push(
                        MenuAction::new("retry-branches", "Retry")
                            .icon("refresh-cw")
                            .keep_open()
                            .into(),
                    );
                }
                Load::Ready(list) => {
                    let needle = query.to_lowercase();
                    let found: Vec<&String> = list
                        .iter()
                        .filter(|b| b.to_lowercase().contains(&needle))
                        .collect();
                    if found.is_empty() {
                        items.push(MenuItem::Note("No matching branches".into()));
                    }
                    for branch in found.iter().take(BRANCH_ROWS) {
                        items.push(
                            MenuAction::new(format!("branch:{branch}"), (*branch).clone())
                                .checked(chosen.as_deref() == Some(branch.as_str()))
                                .into(),
                        );
                    }
                    if found.len() > BRANCH_ROWS {
                        items.push(MenuItem::Note(
                            format!("{} more — type to narrow", found.len() - BRANCH_ROWS).into(),
                        ));
                    }
                }
            }
            items
        })
    }
    fn plain_items(&self, kind: PopupKind) -> Vec<MenuItem> {
        match kind {
            PopupKind::Commits => {
                let Some(range) = self.range.ready() else {
                    return Vec::new();
                };
                let mut items: Vec<MenuItem> = vec![
                    MenuAction::new("all", format!("All commits ({})", range.commits.len()))
                        .checked(self.scope == Scope::All)
                        .into(),
                    MenuItem::Separator,
                ];
                items.extend(range.commits.iter().enumerate().map(|(ix, c)| {
                    MenuAction::new(format!("commit:{ix}"), c.subject.clone())
                        .value(c.short_sha.clone())
                        .checked(self.scope == Scope::Commit(ix))
                        .into()
                }));
                items
            }
            PopupKind::CreateMenu => vec![
                MenuAction::new("draft", "Create Draft Pull Request")
                    .icon("git-pull-request-draft")
                    .disabled(!self.can_create_now())
                    .into(),
            ],
            PopupKind::Templates => self
                .setup
                .ready()
                .map(|s| {
                    s.templates
                        .iter()
                        .enumerate()
                        .map(|(ix, t)| {
                            MenuAction::new(format!("template:{ix}"), t.name.clone())
                                .checked(self.template == Some(ix))
                                .into()
                        })
                        .collect()
                })
                .unwrap_or_default(),
            PopupKind::Base | PopupKind::Head => Vec::new(),
        }
    }
    /// Create's gate without the title, which needs the app to read.
    fn can_create_now(&self) -> bool {
        can_create(
            &self.check,
            &self.push,
            self.busy() || self.generating(),
            "-",
        )
    }

    fn key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" {
            return;
        }
        if self.popup.take().is_some() || self.metadata.read(cx).is_open() {
            cx.notify();
        } else if self.generating() {
            self.cancel_generate(cx);
        } else if !self.busy() {
            self.close(cx);
        }
        cx.stop_propagation();
    }
}

/// The repository part of a branch label: shown only when base and head
/// live in different repositories.
pub(crate) fn branch_label(repo: Option<&PrRepo>, branch: &str, show_owner: bool) -> String {
    match repo {
        Some(repo) if show_owner => format!("{}:{branch}", repo.owner),
        _ => branch.to_owned(),
    }
}
