//! Reviewers, assignees and labels of the selected GitHub pull request, edited
//! in place with the New Pull Request dialog's picker. Each change is saved at
//! once (one save at a time; the latest change waits its turn). A failed save
//! says why and reloads what GitHub has.
//!
//! Also: revealing a pull request the app just created or pointed at, which
//! the list may not hold yet.
use super::*;
use crate::pr_metadata::{CandidateState, PrMetadataEvent, PrMetadataPicker};
use monocode_core::{
    inbox::{Label, Person},
    scm::{self, PrCandidates, PrMetadata, PrRepo},
};
use std::path::Path;

/// The calls behind the editor; tests swap in a fake.
pub(crate) trait MetaBackend: Send + Sync + 'static {
    /// The pull request's metadata and the signed-in login.
    fn metadata(&self, target: &Target) -> Result<(PrMetadata, String), String>;
    fn candidates(&self, target: &Target) -> Result<PrCandidates, String>;
    fn save(&self, target: &Target, metadata: &PrMetadata) -> Result<(), String>;
}

pub(crate) struct Gh;
impl MetaBackend for Gh {
    fn metadata(&self, t: &Target) -> Result<(PrMetadata, String), String> {
        scm::pr_metadata(&t.cwd, &t.host, &t.repo, t.number)
    }
    fn candidates(&self, t: &Target) -> Result<PrCandidates, String> {
        let (owner, name) = t.repo.split_once('/').unwrap_or_default();
        let repo = PrRepo {
            host: t.host.clone(),
            owner: owner.into(),
            name: name.into(),
            ..Default::default()
        };
        scm::pr_candidates(&t.cwd, &repo)
    }
    fn save(&self, t: &Target, metadata: &PrMetadata) -> Result<(), String> {
        scm::pr_set_metadata(&t.cwd, &t.host, &t.repo, t.number, metadata)
    }
}

/// Which pull request, and the checkout the calls run in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub cwd: PathBuf,
    pub host: String,
    /// `owner/name`.
    pub repo: String,
    pub number: i64,
}

impl Target {
    /// GitHub pull requests with a known repository only.
    pub(crate) fn of(item: &Item, fallback_cwd: &Path) -> Option<Self> {
        if item.provider != "github" || item.kind != "pr" || item.number <= 0 {
            return None;
        }
        if !item.repo.contains('/') {
            return None;
        }
        let cwd = if item.project_path.is_empty() {
            fallback_cwd.to_path_buf()
        } else {
            PathBuf::from(&item.project_path)
        };
        Some(Self {
            cwd,
            host: url_host(&item.url).unwrap_or_else(|| "github.com".into()),
            repo: item.repo.clone(),
            number: item.number,
        })
    }
}

/// `host` of `https://host/owner/name/pull/1`.
fn url_host(url: &str) -> Option<String> {
    let rest = url.trim().split_once("://")?.1;
    let host = rest.split('/').next()?.rsplit('@').next()?;
    (!host.is_empty()).then(|| host.to_lowercase())
}

/// The editor of the selected pull request.
pub(crate) struct PrMeta {
    pub key: String,
    pub target: Target,
    pub picker: Entity<PrMetadataPicker>,
    /// Loading or saving failed.
    pub error: Option<String>,
    /// GitHub's values are in; until then the pencils are off.
    pub loaded: bool,
    pub saving: bool,
    queued: Option<PrMetadata>,
    /// Drops load answers for an older load.
    request: u64,
    _subscriptions: Vec<Subscription>,
}

/// A pull request to select once the list holds it (or at once, if it never
/// does: a filter may hide it, or GitHub may not list it yet).
#[derive(Clone, Debug)]
pub(crate) struct RevealPr {
    pub repo: String,
    pub number: i64,
    pub url: String,
    pub title: String,
    pub project: PathBuf,
}

impl RevealPr {
    /// From the pull request's address (`https://host/owner/name/pull/7`).
    pub(crate) fn from_url(url: &str, title: &str, project: PathBuf) -> Option<Self> {
        let rest = url.trim().split_once("://")?.1;
        let mut parts = rest.split(['/', '?', '#']);
        let (_host, owner, name, pull, number) = (
            parts.next()?,
            parts.next()?,
            parts.next()?,
            parts.next()?,
            parts.next()?,
        );
        let number: i64 = number.parse().ok()?;
        (pull == "pull" && !owner.is_empty() && !name.is_empty() && number > 0).then(|| Self {
            repo: format!("{owner}/{name}"),
            number,
            url: url.trim().to_owned(),
            title: title.to_owned(),
            project,
        })
    }

    fn item(&self) -> Item {
        Item {
            provider: "github".into(),
            kind: "pr".into(),
            id: self.url.clone(),
            number: self.number,
            title: self.title.clone(),
            url: self.url.clone(),
            state: "open".into(),
            repo: self.repo.clone(),
            project_path: self.project.to_string_lossy().into_owned(),
            updated_at: chrono_now(),
            created_at: chrono_now(),
            ..Default::default()
        }
    }
}

fn chrono_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

impl Inbox {
    /// Show `pr` selected on the GitHub source, now and after the next refresh.
    pub(crate) fn reveal_pull_request(&mut self, pr: RevealPr, cx: &mut Context<Self>) {
        let item = pr.item();
        if self.state.source != "github" {
            self.state.source = "github".into();
            self.save();
        }
        self.search
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        self.reveal = Some(item.key());
        self.pinned = Some(item);
        self.place_pinned(cx);
        self.refresh(cx);
    }

    /// Keeps the pinned pull request in the list until a fetch brings the real
    /// row, and selects it once.
    pub(super) fn place_pinned(&mut self, cx: &mut Context<Self>) {
        if let Some(pinned) = self.pinned.clone() {
            let key = pinned.key();
            if self.snapshot.items.iter().any(|item| item.key() == key) {
                // The fetched row replaces it from now on.
                self.pinned = None;
            } else {
                self.snapshot.items.insert(0, pinned);
            }
        }
        if let Some(key) = self.reveal.take() {
            self.select(key, cx);
        }
    }

    /// Opens the editor when the selected item is a GitHub pull request.
    pub(super) fn open_pr_meta(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.item() else {
            self.pr_meta = None;
            return;
        };
        let Some(target) = Target::of(&item, &self.service.cwd) else {
            self.pr_meta = None;
            return;
        };
        let key = item.key();
        if self.pr_meta.as_ref().is_some_and(|meta| meta.key == key) {
            return;
        }
        let initial = PrMetadata {
            assignees: item.assignees.iter().map(|p| p.login.clone()).collect(),
            labels: item.labels.iter().map(|l| l.name.clone()).collect(),
            ..Default::default()
        };
        let picker = cx.new(|cx| {
            let mut picker = PrMetadataPicker::new(String::new(), initial);
            picker.set_enabled(false, cx);
            picker
        });
        let subscriptions = vec![
            cx.subscribe(
                &picker,
                |this, _, event: &PrMetadataEvent, cx| match event {
                    PrMetadataEvent::Changed(metadata) => this.save_pr_meta(metadata.clone(), cx),
                    PrMetadataEvent::Retry => this.load_pr_candidates(cx),
                },
            ),
            // Chips change the first summary row's height.
            cx.observe(&picker, |this, _, cx| {
                this.summary_scroll.remeasure_items(0..1);
                cx.notify();
            }),
        ];
        self.pr_meta = Some(PrMeta {
            key,
            target,
            picker,
            error: None,
            loaded: false,
            saving: false,
            queued: None,
            request: 0,
            _subscriptions: subscriptions,
        });
        self.load_pr_meta(cx);
        self.load_pr_candidates(cx);
    }

    /// Reads GitHub's values into the picker (not while a save is pending).
    pub(super) fn load_pr_meta(&mut self, cx: &mut Context<Self>) {
        let Some(meta) = self.pr_meta.as_mut() else {
            return;
        };
        if meta.saving {
            return;
        }
        meta.request += 1;
        let (request, key, target) = (meta.request, meta.key.clone(), meta.target.clone());
        let backend = self.meta_backend.clone();
        let task = cx
            .background_executor()
            .spawn(async move { backend.metadata(&target) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(meta) = this.pr_meta.as_mut() else {
                    return;
                };
                if meta.key != key || meta.request != request || meta.saving {
                    return;
                }
                match result {
                    Ok((metadata, viewer)) => {
                        // Leave an open picker alone; the next load catches up.
                        if meta.picker.read(cx).is_open() && meta.loaded {
                            return;
                        }
                        meta.loaded = true;
                        meta.picker.update(cx, |picker, cx| {
                            picker.set_viewer(viewer, cx);
                            picker.set_metadata(metadata, cx);
                            picker.set_enabled(true, cx);
                        });
                    }
                    Err(error) => {
                        meta.error = Some(format!("Couldn’t load reviewers and labels: {error}"))
                    }
                }
                this.summary_scroll.remeasure_items(0..1);
                cx.notify();
            });
        })
        .detach();
    }

    fn load_pr_candidates(&mut self, cx: &mut Context<Self>) {
        let Some(meta) = self.pr_meta.as_ref() else {
            return;
        };
        let (key, target, picker) = (meta.key.clone(), meta.target.clone(), meta.picker.clone());
        picker.update(cx, |picker, cx| {
            picker.set_candidates(CandidateState::Loading, cx)
        });
        let backend = self.meta_backend.clone();
        let task = cx
            .background_executor()
            .spawn(async move { backend.candidates(&target) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.pr_meta.as_ref().is_none_or(|meta| meta.key != key) {
                    return;
                }
                let state = match result {
                    Ok(candidates) => CandidateState::Ready(candidates),
                    Err(error) => CandidateState::Failed(error),
                };
                picker.update(cx, |picker, cx| picker.set_candidates(state, cx));
            });
        })
        .detach();
    }

    fn save_pr_meta(&mut self, metadata: PrMetadata, cx: &mut Context<Self>) {
        let Some(meta) = self.pr_meta.as_mut() else {
            return;
        };
        if meta.saving {
            meta.queued = Some(metadata);
            return;
        }
        meta.saving = true;
        meta.error = None;
        // A load in flight would bring back the values before this change.
        meta.request += 1;
        let (key, target) = (meta.key.clone(), meta.target.clone());
        let backend = self.meta_backend.clone();
        let task = cx.background_executor().spawn({
            let metadata = metadata.clone();
            async move { backend.save(&target, &metadata) }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                let Some(meta) = this.pr_meta.as_mut() else {
                    return;
                };
                if meta.key != key {
                    return;
                }
                meta.saving = false;
                match result {
                    Ok(()) => {
                        let queued = meta.queued.take();
                        this.show_saved_metadata(&key, &metadata, cx);
                        if let Some(next) = queued {
                            this.save_pr_meta(next, cx);
                        }
                    }
                    Err(error) => {
                        meta.queued = None;
                        meta.error = Some(format!("Couldn’t save: {error}"));
                        this.load_pr_meta(cx);
                    }
                }
                this.summary_scroll.remeasure_items(0..1);
                cx.notify();
            });
        })
        .detach();
    }

    /// The list row and header follow a save without waiting for a refresh.
    fn show_saved_metadata(&mut self, key: &str, metadata: &PrMetadata, cx: &App) {
        let candidates = match self
            .pr_meta
            .as_ref()
            .map(|m| m.picker.read(cx).candidates())
        {
            Some(CandidateState::Ready(candidates)) => Some(candidates.clone()),
            _ => None,
        };
        let Some(item) = self.snapshot.items.iter_mut().find(|i| i.key() == key) else {
            return;
        };
        item.assignees = (metadata.assignees.iter())
            .map(|login| Person {
                login: login.clone(),
                avatar_url: (item.assignees.iter())
                    .find(|p| &p.login == login)
                    .map(|p| p.avatar_url.clone())
                    .or_else(|| {
                        candidates
                            .as_ref()?
                            .users
                            .iter()
                            .find(|u| &u.login == login)
                            .map(|u| u.avatar_url.clone())
                    })
                    .unwrap_or_default(),
            })
            .collect();
        item.labels = (metadata.labels.iter())
            .map(|name| {
                (item.labels.iter().find(|l| &l.name == name).cloned())
                    .or_else(|| {
                        candidates
                            .as_ref()?
                            .labels
                            .iter()
                            .find(|l| &l.name == name)
                            .cloned()
                    })
                    .unwrap_or_else(|| Label {
                        name: name.clone(),
                        color: String::new(),
                    })
            })
            .collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_metadata::Field;
    use gpui::TestAppContext;
    use monocode_core::scm::PrUser;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Fake {
        current: Mutex<PrMetadata>,
        saves: Mutex<Vec<PrMetadata>>,
        fail_save: Mutex<Option<String>>,
    }
    impl MetaBackend for Fake {
        fn metadata(&self, _: &Target) -> Result<(PrMetadata, String), String> {
            Ok((self.current.lock().unwrap().clone(), "me".into()))
        }
        fn candidates(&self, _: &Target) -> Result<PrCandidates, String> {
            Ok(PrCandidates {
                users: vec![PrUser {
                    login: "ada".into(),
                    name: "Ada".into(),
                    avatar_url: "https://avatars/ada".into(),
                }],
                teams: vec![],
                labels: vec![Label {
                    name: "bug".into(),
                    color: "d73a4a".into(),
                }],
            })
        }
        fn save(&self, _: &Target, metadata: &PrMetadata) -> Result<(), String> {
            self.saves.lock().unwrap().push(metadata.clone());
            if let Some(error) = self.fail_save.lock().unwrap().clone() {
                return Err(error);
            }
            *self.current.lock().unwrap() = metadata.clone();
            Ok(())
        }
    }

    fn pr(number: i64) -> Item {
        Item {
            provider: "github".into(),
            kind: "pr".into(),
            state: "open".into(),
            repo: "acme/app".into(),
            number,
            url: format!("https://ghe.acme.io/acme/app/pull/{number}"),
            project_path: "/work/app".into(),
            ..Default::default()
        }
    }

    fn inbox(cx: &mut TestAppContext, fake: Arc<Fake>) -> Entity<Inbox> {
        cx.update(input::bind);
        cx.update(crate::popover::bind);
        cx.update(|cx| {
            cx.new(|cx| {
                let store = Arc::new(std::sync::Mutex::new(
                    monocode_core::store::Store::open(Path::new(":memory:")).unwrap(),
                ));
                let mut inbox =
                    Inbox::new_inner(store, vec![], PathBuf::from("/work/app"), false, cx);
                inbox.meta_backend = fake;
                // Keep list fetches queued instead of reaching real providers.
                inbox.loading = true;
                inbox
            })
        })
    }

    #[test]
    fn only_github_pull_requests_are_edited_and_the_host_comes_from_the_url() {
        let target = Target::of(&pr(4), Path::new("/fallback")).unwrap();
        assert_eq!(target.host, "ghe.acme.io");
        assert_eq!(target.repo, "acme/app");
        assert_eq!(target.cwd, PathBuf::from("/work/app"));
        let mut issue = pr(4);
        issue.kind = "issue".into();
        assert!(Target::of(&issue, Path::new("/")).is_none());
        let mut bare = pr(4);
        bare.url.clear();
        bare.project_path.clear();
        let target = Target::of(&bare, Path::new("/fallback")).unwrap();
        assert_eq!(
            (target.host.as_str(), target.cwd),
            ("github.com", PathBuf::from("/fallback"))
        );
    }

    #[gpui::test]
    fn changes_save_at_once_one_at_a_time_and_update_the_row(cx: &mut TestAppContext) {
        let fake = Arc::new(Fake::default());
        *fake.current.lock().unwrap() = PrMetadata {
            reviewers: vec!["bob".into()],
            ..Default::default()
        };
        let inbox = inbox(cx, fake.clone());
        inbox.update(cx, |inbox, cx| {
            inbox.snapshot.items = vec![pr(4)];
            inbox.selected = Some(pr(4).key());
            inbox.open_pr_meta(cx);
            let meta = inbox.pr_meta.as_ref().unwrap();
            assert!(!meta.loaded, "the pencils wait for GitHub's values");
        });
        cx.run_until_parked();
        let picker = inbox.read_with(cx, |inbox, _| {
            let meta = inbox.pr_meta.as_ref().unwrap();
            assert!(meta.loaded);
            meta.picker.clone()
        });
        picker.read_with(cx, |picker, _| {
            assert_eq!(picker.metadata().reviewers, ["bob"])
        });
        // Two quick changes: the second waits for the first.
        inbox.update(cx, |inbox, cx| {
            let first = PrMetadata {
                reviewers: vec!["bob".into()],
                labels: vec!["bug".into()],
                ..Default::default()
            };
            let second = PrMetadata {
                assignees: vec!["ada".into()],
                ..first.clone()
            };
            inbox.save_pr_meta(first, cx);
            inbox.save_pr_meta(second, cx);
            assert!(inbox.pr_meta.as_ref().unwrap().saving);
        });
        cx.run_until_parked();
        assert_eq!(fake.saves.lock().unwrap().len(), 2);
        inbox.read_with(cx, |inbox, _| {
            let item = &inbox.snapshot.items[0];
            assert_eq!(item.labels[0].color, "d73a4a");
            assert_eq!(item.assignees[0].avatar_url, "https://avatars/ada");
            assert!(inbox.pr_meta.as_ref().unwrap().error.is_none());
        });
    }

    #[gpui::test]
    fn a_failed_save_says_why_and_reloads_githubs_values(cx: &mut TestAppContext) {
        let fake = Arc::new(Fake::default());
        let inbox = inbox(cx, fake.clone());
        inbox.update(cx, |inbox, cx| {
            inbox.snapshot.items = vec![pr(4)];
            inbox.selected = Some(pr(4).key());
            inbox.open_pr_meta(cx);
        });
        cx.run_until_parked();
        *fake.fail_save.lock().unwrap() = Some("forbidden".into());
        let picker = inbox.read_with(cx, |inbox, _| {
            inbox.pr_meta.as_ref().unwrap().picker.clone()
        });
        cx.update(|cx| {
            picker.update(cx, |picker, cx| {
                let mut metadata = picker.metadata().clone();
                crate::pr_metadata::toggle(&mut metadata, Field::Labels, "label:bug");
                picker.set_metadata(metadata.clone(), cx);
                cx.emit(PrMetadataEvent::Changed(metadata));
            })
        });
        cx.run_until_parked();
        inbox.read_with(cx, |inbox, cx| {
            let meta = inbox.pr_meta.as_ref().unwrap();
            assert_eq!(meta.error.as_deref(), Some("Couldn’t save: forbidden"));
            assert!(meta.picker.read(cx).metadata().labels.is_empty());
            assert!(inbox.snapshot.items[0].labels.is_empty());
        });
    }

    #[test]
    fn a_pull_request_address_names_its_repository_and_number() {
        let pr = RevealPr::from_url(
            "https://ghe.acme.io/up/app/pull/12#top",
            "T",
            "/work/app".into(),
        )
        .unwrap();
        assert_eq!((pr.repo.as_str(), pr.number), ("up/app", 12));
        assert!(
            RevealPr::from_url("https://github.com/up/app/issues/12", "", "/".into()).is_none()
        );
        assert!(RevealPr::from_url("not a url", "", "/".into()).is_none());
    }

    #[gpui::test]
    fn a_revealed_pull_request_is_selected_before_and_after_the_list_has_it(
        cx: &mut TestAppContext,
    ) {
        let inbox = inbox(cx, Arc::new(Fake::default()));
        let reveal = RevealPr {
            repo: "acme/app".into(),
            number: 9,
            url: "https://github.com/acme/app/pull/9".into(),
            title: "New thing".into(),
            project: "/work/app".into(),
        };
        inbox.update(cx, |inbox, cx| {
            inbox.state.source = "linear".into();
            inbox.reveal_pull_request(reveal.clone(), cx);
            assert_eq!(inbox.state.source, "github");
            let item = inbox.item().unwrap();
            assert_eq!((item.number, item.title.as_str()), (9, "New thing"));
            assert_eq!(inbox.pr_meta.as_ref().unwrap().target.number, 9);
            // A fetch that does not list it yet keeps it.
            inbox.snapshot.items = vec![pr(4)];
            inbox.place_pinned(cx);
            assert_eq!(inbox.item().unwrap().number, 9);
            // The fetched row takes over.
            let mut fetched = pr(9);
            fetched.title = "New thing (fetched)".into();
            inbox.snapshot.items = vec![fetched, pr(4)];
            inbox.place_pinned(cx);
            assert!(inbox.pinned.is_none());
            assert_eq!(inbox.item().unwrap().title, "New thing (fetched)");
            assert_eq!(inbox.snapshot.items.len(), 2);
        });
    }
}
