use super::flow::{PushPlan, humanize_branch};
use super::*;
use gpui::{TestAppContext, VisualTestContext, WindowHandle, px};
use monocode_core::scm::{FileDiff, IncomingCommit, PrCandidates, PrTemplate};
use std::{
    cell::RefCell,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering::SeqCst},
    },
};

fn repo(remote: &str, owner: &str) -> PrRepo {
    PrRepo {
        remote: remote.into(),
        host: "github.com".into(),
        owner: owner.into(),
        name: "app".into(),
        default_branch: "main".into(),
        parent: None,
        can_push: true,
    }
}
fn commit(sha: &str, subject: &str) -> IncomingCommit {
    IncomingCommit {
        sha: sha.into(),
        short_sha: sha.chars().take(7).collect(),
        author: "t".into(),
        timestamp: 0,
        subject: subject.into(),
    }
}
fn file(relative: &str) -> ChangedFile {
    ChangedFile {
        path: relative.into(),
        relative: relative.into(),
        status: "modified".into(),
        additions: 1,
        deletions: 0,
        staged: false,
        unstaged: false,
    }
}
fn range(commits: Vec<IncomingCommit>) -> PrRange {
    PrRange {
        base_sha: "base".into(),
        head_sha: "head".into(),
        merge_base: "mb".into(),
        commits,
        files: vec![file("src/a.rs"), file("README.md")],
    }
}

/// Git and GitHub as a test wants them, counting the calls that change things.
struct Fake {
    setup: PrSetup,
    range: Mutex<PrRange>,
    push: Mutex<PrPush>,
    check: Mutex<PrBranchCheck>,
    /// The next this-many metadata calls fail.
    metadata_failures: AtomicUsize,
    pushes: AtomicUsize,
    opens: AtomicUsize,
    metadata_calls: AtomicUsize,
}
impl Fake {
    fn new() -> Self {
        Self {
            setup: PrSetup {
                repos: vec![repo("origin", "acme")],
                base_repo: 0,
                head_repo: 0,
                base_branch: "main".into(),
                head_branch: "feature/foo-bar".into(),
                viewer: "me".into(),
                templates: vec![
                    PrTemplate {
                        name: "pull_request_template.md".into(),
                        body: "## Summary\n".into(),
                    },
                    PrTemplate {
                        name: "bug.md".into(),
                        body: "## Bug\n".into(),
                    },
                ],
            },
            range: Mutex::new(range(vec![
                commit("aaaaaaaa1", "First"),
                commit("bbbbbbbb2", "Second"),
            ])),
            push: Mutex::new(PrPush::Create {
                remote: "origin".into(),
                branch: "feature/foo-bar".into(),
            }),
            check: Mutex::new(PrBranchCheck::Ok),
            metadata_failures: AtomicUsize::new(0),
            pushes: AtomicUsize::new(0),
            opens: AtomicUsize::new(0),
            metadata_calls: AtomicUsize::new(0),
        }
    }
}
impl PrBackend for Fake {
    fn setup(&self, _: &Path) -> Result<PrSetup, String> {
        Ok(self.setup.clone())
    }
    fn fetch(&self, _: &Path, _: &[String]) -> Result<(), String> {
        Ok(())
    }
    fn branches(&self, _: &Path, _: &PrRepo) -> Result<Vec<String>, String> {
        Ok(vec!["main".into(), "develop".into()])
    }
    fn local_branches(&self, _: &Path) -> Result<Vec<String>, String> {
        Ok(vec!["feature/foo-bar".into(), "main".into()])
    }
    fn range(&self, _: &Path, _: &str, _: &str) -> Result<PrRange, String> {
        Ok(self.range.lock().unwrap().clone())
    }
    fn commit_files(&self, _: &Path, _: &str) -> Result<Vec<ChangedFile>, String> {
        Ok(vec![file("src/a.rs")])
    }
    fn file_diff(&self, _: &Path, _: &str, _: &str, path: &str) -> Result<FileDiff, String> {
        Ok(FileDiff {
            path: path.into(),
            relative: path.into(),
            status: "modified".into(),
            original: "a\n".into(),
            current: "b\n".into(),
            binary: false,
            too_large: false,
        })
    }
    fn push_state(&self, _: &Path, _: &str, _: &str, _: &str) -> Result<PrPush, String> {
        Ok(self.push.lock().unwrap().clone())
    }
    fn check(&self, _: &Path, _: &CheckRequest, _: &PrRange) -> Result<PrBranchCheck, String> {
        Ok(self.check.lock().unwrap().clone())
    }
    fn candidates(&self, _: &Path, _: &PrRepo) -> Result<PrCandidates, String> {
        Ok(PrCandidates::default())
    }
    fn push(&self, _: &Path, _: &PushPlan) -> Result<(), String> {
        self.pushes.fetch_add(1, SeqCst);
        Ok(())
    }
    fn open(&self, _: &Path, submit: &PrSubmit) -> Result<CreatedPr, String> {
        self.opens.fetch_add(1, SeqCst);
        Ok(CreatedPr {
            number: 7,
            url: "https://github.com/acme/app/pull/7".into(),
            node_id: "PR_7".into(),
            repo: submit.base.slug(),
            host: "github.com".into(),
        })
    }
    fn apply_metadata(&self, _: &Path, _: &CreatedPr, _: &PrMetadata) -> Result<(), String> {
        self.metadata_calls.fetch_add(1, SeqCst);
        let left = self.metadata_failures.load(SeqCst);
        if left > 0 {
            self.metadata_failures.store(left - 1, SeqCst);
            return Err("label not found".into());
        }
        Ok(())
    }
}

fn open(
    cx: &mut TestAppContext,
    fake: Arc<Fake>,
    draft: Option<PrDraft>,
) -> WindowHandle<PrDialog> {
    cx.update(input::bind);
    cx.update(crate::popover::bind);
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            cx.new(|cx| PrDialog::new("/tmp/pr-test".into(), fake, draft, None, window, cx))
        })
        .unwrap()
    });
    cx.run_until_parked();
    window
}
fn events(cx: &mut TestAppContext, window: WindowHandle<PrDialog>) -> Rc<RefCell<Vec<String>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let dialog = window.root(cx).unwrap();
    let log = seen.clone();
    cx.update(|cx| {
        cx.subscribe(&dialog, move |_, event: &PrDialogEvent, _| {
            log.borrow_mut().push(match event {
                PrDialogEvent::Close(draft) => format!("close:{}", draft.title),
                PrDialogEvent::Created(pr) => format!("created:{}", pr.number),
                PrDialogEvent::ViewExisting(pr) => format!("view:{}", pr.number),
            })
        })
        .detach()
    });
    seen
}
fn title(window: WindowHandle<PrDialog>, cx: &mut TestAppContext) -> String {
    window
        .update(cx, |d, _, cx| d.title.read(cx).content.to_string())
        .unwrap()
}

#[test]
fn branch_names_read_as_titles() {
    assert_eq!(humanize_branch("feature/foo-bar"), "Foo bar");
    assert_eq!(humanize_branch("fix_login_crash"), "Fix login crash");
    assert_eq!(humanize_branch("user/JIRA-12-make.it"), "JIRA 12 make it");
    assert_eq!(humanize_branch("main"), "Main");
    assert_eq!(humanize_branch(""), "");
    assert_eq!(
        suggested_title(Some(&[commit("a", "  Add the thing ")]), "feature/x"),
        "Add the thing"
    );
    assert_eq!(
        suggested_title(Some(&[commit("a", "One"), commit("b", "Two")]), "feature/x"),
        "X"
    );
    assert_eq!(effective_title("  ", "feature/foo-bar"), "Foo bar");
    assert_eq!(effective_title(" Mine ", "feature/foo-bar"), "Mine");
}

#[test]
fn create_is_on_only_when_the_branches_check_out_the_push_can_go_and_nothing_runs() {
    let ok = Load::Ready(PrBranchCheck::Ok);
    let pushable = [
        Load::Ready(PrPush::UpToDate),
        Load::Ready(PrPush::Create {
            remote: "origin".into(),
            branch: "x".into(),
        }),
        Load::Ready(PrPush::Sync {
            remote: "origin".into(),
            branch: "x".into(),
        }),
    ];
    for push in &pushable {
        assert!(can_create(&ok, push, false, "Title"));
        assert!(!can_create(&ok, push, true, "Title"), "busy");
        assert!(!can_create(&ok, push, false, "  "), "no title");
    }
    let diverged = Load::Ready(PrPush::Diverged {
        remote: "origin".into(),
        branch: "x".into(),
    });
    assert!(!can_create(&ok, &diverged, false, "Title"));
    assert!(!can_create(&ok, &Load::Loading, false, "Title"));
    assert!(!can_create(&ok, &Load::Failed("x".into()), false, "Title"));
    let up = Load::Ready(PrPush::UpToDate);
    for check in [
        Load::Loading,
        Load::Failed("offline".into()),
        Load::Ready(PrBranchCheck::NoChanges),
        Load::Ready(PrBranchCheck::AlreadyExists {
            pr: Pr {
                number: 3,
                title: String::new(),
                url: String::new(),
                state: "open".into(),
            },
        }),
    ] {
        assert!(!can_create(&check, &up, false, "Title"));
    }
}

#[test]
fn a_metadata_failure_keeps_the_pull_request_and_retry_only_sets_the_metadata() {
    let fake = Fake::new();
    fake.metadata_failures.store(1, SeqCst);
    let mut submission = Submission {
        cwd: "/tmp".into(),
        push: push_plan("feature/foo-bar", &fake.push.lock().unwrap()),
        submit: PrSubmit {
            base: repo("origin", "acme"),
            base_branch: "main".into(),
            head: repo("origin", "acme"),
            head_branch: "feature/foo-bar".into(),
            title: "Foo bar".into(),
            body: String::new(),
            draft: false,
        },
        metadata: PrMetadata {
            labels: vec!["bug".into()],
            ..Default::default()
        },
        created: None,
    };
    assert_eq!(
        submission.stages(),
        [Stage::Pushing, Stage::Creating, Stage::Metadata]
    );
    let Outcome::MetadataFailed(pr, error) = submission.run_all(&fake) else {
        panic!("the metadata should fail");
    };
    assert_eq!((pr.number, error.as_str()), (7, "label not found"));
    // The retry runs the metadata step alone.
    assert_eq!(submission.stages(), [Stage::Metadata]);
    assert_eq!(submission.run_all(&fake), Outcome::Created(pr));
    assert_eq!(fake.opens.load(SeqCst), 1);
    assert_eq!(fake.pushes.load(SeqCst), 1);
    assert_eq!(fake.metadata_calls.load(SeqCst), 2);
    // No metadata, nothing to push: just create.
    let mut plain = Submission {
        push: None,
        metadata: PrMetadata::default(),
        created: None,
        ..submission
    };
    assert_eq!(plain.stages(), [Stage::Creating]);
    assert!(matches!(plain.run_all(&fake), Outcome::Created(_)));
}

#[gpui::test]
fn the_dialog_retries_metadata_on_the_pull_request_it_already_created(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::new());
    fake.metadata_failures.store(1, SeqCst);
    let window = open(cx, fake.clone(), None);
    let seen = events(cx, window);
    window
        .update(cx, |dialog, _, cx| {
            assert!(
                dialog.can_create(cx),
                "check {:?} push {:?}",
                dialog.check,
                dialog.push
            );
            dialog.metadata.update(cx, |picker, cx| {
                let mut metadata = picker.metadata().clone();
                metadata.labels.push("bug".into());
                picker.set_metadata(metadata, cx);
            });
            dialog.create(false, cx);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |dialog, _, _| {
            assert!(matches!(
                &dialog.submit,
                SubmitState::MetadataFailed { pr, .. } if pr.number == 7
            ));
        })
        .unwrap();
    assert!(seen.borrow().is_empty());
    window
        .update(cx, |dialog, _, cx| dialog.retry_metadata(cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(*seen.borrow(), ["created:7"]);
    assert_eq!(fake.opens.load(SeqCst), 1);
    assert_eq!(fake.pushes.load(SeqCst), 1);
    assert_eq!(fake.metadata_calls.load(SeqCst), 2);
}

#[gpui::test]
fn the_title_follows_the_range_until_the_user_writes_one(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::new());
    let window = open(cx, fake.clone(), None);
    // Two commits: the branch in words. The default template fills the body.
    assert_eq!(title(window, cx), "Foo bar");
    window
        .update(cx, |dialog, _, cx| {
            assert_eq!(dialog.body.read(cx).content.as_ref(), "## Summary\n");
            assert!(!dialog.title_touched);
        })
        .unwrap();
    // One commit: its subject.
    *fake.range.lock().unwrap() = range(vec![commit("cccccccc3", "Fix the crash")]);
    window
        .update(cx, |dialog, _, cx| dialog.reload(cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(title(window, cx), "Fix the crash");
    // Typed: the range no longer moves it.
    window
        .update(cx, |dialog, _, cx| {
            dialog
                .title
                .update(cx, |input, cx| input.set_text("My title".into(), cx))
        })
        .unwrap();
    cx.run_until_parked();
    *fake.range.lock().unwrap() = range(vec![commit("a", "One"), commit("b", "Two")]);
    window
        .update(cx, |dialog, _, cx| dialog.reload(cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(title(window, cx), "My title");
    window
        .update(cx, |dialog, _, _| assert!(dialog.title_touched))
        .unwrap();
}

#[gpui::test]
fn results_for_an_older_base_or_head_are_dropped(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::new());
    let window = open(cx, fake.clone(), None);
    window
        .update(cx, |dialog, _, cx| {
            assert!(dialog.range.ready().is_some());
            let stale = dialog.generation;
            dialog.set_base(0, "develop".into(), cx);
            assert_eq!(dialog.generation, stale + 1);
            // The old base's range arrives late: ignored.
            dialog.apply_range(stale, Ok(range(vec![commit("z", "Stale")])), cx);
            assert_eq!(dialog.range, Load::Loading);
            let check = dialog.check_generation;
            dialog.apply_check(
                check.wrapping_sub(1),
                Ok(PrPush::UpToDate),
                Ok(PrBranchCheck::NoChanges),
                cx,
            );
            assert_eq!(dialog.check, Load::Loading);
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |dialog, _, _| {
            assert_eq!(dialog.base_branch, "develop");
            let range = dialog.range.ready().expect("the new base's range");
            assert_eq!(range.commits[0].subject, "First");
            assert_eq!(dialog.check, Load::Ready(PrBranchCheck::Ok));
        })
        .unwrap();
}

#[gpui::test]
fn the_check_gates_create_and_says_why(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::new());
    *fake.check.lock().unwrap() = PrBranchCheck::AlreadyExists {
        pr: Pr {
            number: 12,
            title: "Old".into(),
            url: "https://github.com/acme/app/pull/12".into(),
            state: "open".into(),
        },
    };
    let window = open(cx, fake.clone(), None);
    window
        .update(cx, |dialog, _, cx| assert!(!dialog.can_create(cx)))
        .unwrap();
    *fake.check.lock().unwrap() = PrBranchCheck::Ok;
    *fake.push.lock().unwrap() = PrPush::Diverged {
        remote: "origin".into(),
        branch: "feature/foo-bar".into(),
    };
    window
        .update(cx, |dialog, _, cx| dialog.recheck(cx))
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |dialog, _, cx| {
            assert!(!dialog.can_create(cx));
            dialog.create(false, cx);
        })
        .unwrap();
    cx.run_until_parked();
    assert_eq!(fake.opens.load(SeqCst), 0);
}

#[gpui::test]
fn cancel_keeps_a_draft_that_the_next_dialog_starts_from(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::new());
    let window = open(cx, fake.clone(), None);
    let seen = events(cx, window);
    let draft = window
        .update(cx, |dialog, _, cx| {
            dialog
                .title
                .update(cx, |input, cx| input.set_text("Typed".into(), cx));
            dialog
                .body
                .update(cx, |input, cx| input.set_text("Body".into(), cx));
            dialog.set_base(0, "develop".into(), cx);
            dialog.draft(cx)
        })
        .unwrap();
    cx.run_until_parked();
    window.update(cx, |dialog, _, cx| dialog.close(cx)).unwrap();
    cx.run_until_parked();
    assert_eq!(*seen.borrow(), ["close:Typed"]);
    assert_eq!(draft.base, Some(("acme/app".into(), "develop".into())));
    let again = open(
        cx,
        fake,
        Some(PrDraft {
            title_touched: true,
            ..draft
        }),
    );
    again
        .update(cx, |dialog, _, cx| {
            assert_eq!(dialog.title.read(cx).content.as_ref(), "Typed");
            assert_eq!(dialog.body.read(cx).content.as_ref(), "Body");
            assert_eq!(dialog.base_branch, "develop");
            assert!(dialog.title_touched);
        })
        .unwrap();
}

#[gpui::test]
fn a_commit_scope_lists_that_commits_files(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::new());
    let window = open(cx, fake, None);
    window
        .update(cx, |dialog, _, cx| {
            assert_eq!(dialog.file_list().len(), 2);
            assert_eq!(dialog.selected.as_deref(), Some("src/a.rs"));
            dialog.set_scope(Scope::Commit(1), cx);
            assert_eq!(
                dialog.diff_endpoints(),
                Some(("bbbbbbbb2^".into(), "bbbbbbbb2".into()))
            );
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |dialog, _, _| {
            assert_eq!(dialog.file_list().len(), 1);
            assert_eq!(dialog.selected.as_deref(), Some("src/a.rs"));
        })
        .unwrap();
}

#[gpui::test]
fn the_dialog_remembers_the_size_it_was_dragged_to(cx: &mut TestAppContext) {
    use gpui::{MouseButton, point};
    *REMEMBERED_SIZE.lock().unwrap() = Default::default();
    let fake = Arc::new(Fake::new());
    let window = open(cx, fake.clone(), None);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let card = visual.debug_bounds("pr-card").expect("the dialog");
    let grip = visual.debug_bounds("pr-resize").unwrap().center();
    visual.simulate_mouse_down(grip, MouseButton::Left, Default::default());
    let to = grip - point(px(40.), px(30.));
    visual.simulate_mouse_move(to, MouseButton::Left, Default::default());
    visual.simulate_mouse_up(to, MouseButton::Left, Default::default());
    visual.run_until_parked();
    let smaller = visual.debug_bounds("pr-card").unwrap().size;
    assert_eq!(smaller.width, card.size.width - px(80.));
    assert_eq!(smaller.height, card.size.height - px(60.));
    let again = open(cx, fake, None);
    let visual = &mut VisualTestContext::from_window(again.into(), cx);
    visual.run_until_parked();
    assert_eq!(visual.debug_bounds("pr-card").unwrap().size, smaller);
    *REMEMBERED_SIZE.lock().unwrap() = Default::default();
}

#[gpui::test]
fn every_part_of_the_dialog_is_laid_out_inside_the_card(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::new());
    let window = open(cx, fake, None);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let card = visual.debug_bounds("pr-card").expect("the dialog");
    let part = |visual: &mut VisualTestContext, id: &'static str| {
        let bounds = visual
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("{id} is drawn"));
        assert!(
            bounds.size.width > px(0.) && bounds.size.height > px(0.),
            "{id} has room: {bounds:?}"
        );
        assert!(
            card.contains(&bounds.origin) && card.contains(&bounds.bottom_right()),
            "{id} sits in the card: {bounds:?} in {card:?}"
        );
        bounds
    };
    let base = part(visual, "pr-base");
    let head = part(visual, "pr-head");
    assert!(
        base.right() <= head.left(),
        "base ← head reads left to right"
    );
    // Create on a branch the remote lacks: the warning and the name field.
    part(visual, "pr-push-note");
    part(visual, "pr-remote-branch");
    let scope = part(visual, "pr-scope");
    let diff = part(visual, "pr-diff");
    assert!(scope.right() <= diff.left() + px(1.));
    let title = part(visual, "pr-title");
    let description = part(visual, "pr-description");
    assert!(diff.bottom() <= title.top() && title.bottom() <= description.top());
    part(visual, "pr-generate");
    part(visual, "pr-template");
    for field in ["reviewers", "assignees", "labels"] {
        let row = part(visual, format!("pr-meta-{field}").leak());
        assert!(row.left() >= description.right());
        part(visual, format!("pr-meta-{field}-edit").leak());
    }
    let create = part(visual, "pr-create");
    let chevron = part(visual, "pr-create-options");
    assert!(create.right() <= chevron.left() + px(1.));
    assert!(description.bottom() <= create.top());
}

#[gpui::test]
fn the_base_picker_lists_the_repos_branches_and_picking_one_reloads(cx: &mut TestAppContext) {
    let fake = Arc::new(Fake::new());
    let window = open(cx, fake, None);
    window
        .update(cx, |dialog, window, cx| {
            dialog.toggle_popup(PopupKind::Base, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |dialog, _, cx| {
            let Some(Popup {
                menu: PopupMenu::Search(menu),
                ..
            }) = &dialog.popup
            else {
                panic!("the base picker is open");
            };
            let ids: Vec<_> = menu
                .read(cx)
                .rows(cx)
                .iter()
                .filter_map(|row| match row {
                    MenuItem::Action(a) => Some((a.id.to_string(), a.checked)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                ids,
                [
                    ("branch:main".to_string(), true),
                    ("branch:develop".to_string(), false)
                ]
            );
            menu.update(cx, |_, cx| {
                cx.emit(MenuEvent::Picked("branch:develop".into(), None))
            });
        })
        .unwrap();
    cx.run_until_parked();
    window
        .update(cx, |dialog, _, _| {
            assert!(dialog.popup.is_none());
            assert_eq!(dialog.base_branch, "develop");
            assert!(dialog.range.ready().is_some());
        })
        .unwrap();
}
