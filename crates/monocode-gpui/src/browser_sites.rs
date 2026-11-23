//! Site permissions and certificate errors in a page (docs/native/browser/
//! context.md K55, K62, K63). The decisions are monocode-core's
//! (`browser::sites`); this is where they meet the page and the user:
//!
//! - A permission request is answered from the profile's remembered
//!   settings; refused while an agent drives the page out of the user's
//!   sight (K63); or asked in a bar under the toolbar, which never covers the
//!   page (K44). Requests wait their turn there. Every answer is noted for
//!   the agent tools (`permissionRequest` in action effects).
//! - Camera and microphone also need macOS's consent: after the site is
//!   allowed, the app asks the system before the page gets them.
//! - A certificate error on a local or private host is taken; elsewhere the
//!   page shows a warning instead of the page, with "Back to safety" and
//!   "Proceed (unsafe)", which holds for the host and certificate until the
//!   app quits. The address bar says "Not secure" for such hosts.
//! - The site-info button in the address bar lists the site's permissions,
//!   each with a reset; Settings lists every site's ([`all_sites`]).
use super::*;
use monocode_core::browser::sites::{
    self, CertBlock, CertDecision, CertExceptions, Decision, Outcome, PermissionNote, Setting,
    SitePermissions,
};
use std::collections::HashSet;

/// Notes a page keeps for agents.
const NOTES: usize = 20;

thread_local! {
    /// Every profile's answers, by the profile's folder (or a throwaway
    /// profile's name), loaded once.
    static STORES: RefCell<HashMap<PathBuf, Rc<RefCell<SitePermissions>>>> =
        RefCell::new(HashMap::new());
    /// Profiles whose leftover grants of an earlier run were taken back.
    static SWEPT: RefCell<HashSet<PathBuf>> = RefCell::new(HashSet::new());
    /// The user's "Proceed (unsafe)" of this run (K62).
    static EXCEPTIONS: RefCell<CertExceptions> = RefCell::new(CertExceptions::default());
    /// Hosts whose untrusted certificate was taken this run, local ones
    /// included: their address bar says "Not secure".
    static INSECURE: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

fn next_seq() -> u64 {
    sites::next_note_seq()
}

/// The folder of `profile` under the browser root `root`; a throwaway
/// profile has none.
pub(crate) fn profile_dir(root: &Path, profile: &monocode_cef::Profile) -> Option<PathBuf> {
    let profiles = root.join("Profiles");
    match profile {
        monocode_cef::Profile::Default => Some(profiles.join("default")),
        monocode_cef::Profile::Isolated(id) => Some(profiles.join(id)),
        monocode_cef::Profile::Ephemeral(_) => None,
    }
}

/// The profile a folder under `root/Profiles` is.
fn profile_of(dir: &Path) -> monocode_cef::Profile {
    match dir.file_name().and_then(|n| n.to_str()) {
        Some("default") | None => monocode_cef::Profile::Default,
        Some(id) => monocode_cef::Profile::Isolated(id.to_owned()),
    }
}

/// A profile's answers, shared by its pages and Settings.
fn store(root: &Path, profile: &monocode_cef::Profile) -> Rc<RefCell<SitePermissions>> {
    let key = match (profile_dir(root, profile), profile) {
        (Some(dir), _) => dir,
        (None, monocode_cef::Profile::Ephemeral(name)) => {
            PathBuf::from(format!("ephemeral:{name}"))
        }
        (None, _) => PathBuf::from("ephemeral:"),
    };
    STORES.with(|stores| {
        (stores.borrow_mut().entry(key.clone()))
            .or_insert_with(|| {
                Rc::new(RefCell::new(match profile_dir(root, profile) {
                    Some(dir) => SitePermissions::load(&dir),
                    None => SitePermissions::in_memory(),
                }))
            })
            .clone()
    })
}

/// [`store`], for a page while Chromium runs: grants an earlier run gave
/// for a page only, which Chromium still holds, are taken back first.
fn page_store(root: &Path, profile: &monocode_cef::Profile) -> Rc<RefCell<SitePermissions>> {
    let store = store(root, profile);
    if let Some(dir) = profile_dir(root, profile)
        && SWEPT.with(|s| s.borrow_mut().insert(dir))
    {
        for (origin, name) in store.borrow_mut().take_temporary() {
            monocode_cef::reset_site_permission(profile, &origin, &name);
        }
    }
    store
}

/// A site's origin and its remembered answers.
pub(crate) type SiteAnswers = (String, Vec<(String, Setting)>);

/// Every remembered answer of every profile under `root`: per profile
/// folder, its sites and their answers. For Settings.
pub(crate) fn all_sites(root: &Path) -> Vec<(PathBuf, Vec<SiteAnswers>)> {
    let Ok(entries) = std::fs::read_dir(root.join("Profiles")) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join(sites::FILE_NAME).exists())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .filter_map(|dir| {
            let sites = store(root, &profile_of(&dir)).borrow().sites();
            (!sites.is_empty()).then_some((dir, sites))
        })
        .collect()
}

/// Forgets a remembered answer (Settings, the site-info list): the site is
/// asked again.
pub(crate) fn forget(root: &Path, dir: &Path, origin: &str, name: &str) {
    let profile = profile_of(dir);
    store(root, &profile).borrow_mut().remove(origin, name);
    monocode_cef::reset_site_permission(&profile, origin, name);
}

/// Whether the address bar says "Not secure" for `url`: an https page
/// whose host's untrusted certificate was taken this run.
fn insecure(url: &str) -> bool {
    url.starts_with("https:") && INSECURE.with(|i| i.borrow().contains(&sites::host_of(url)))
}

/// A permission request the user is asked about.
struct Waiting {
    request: monocode_cef::PermissionRequest,
    origin: String,
    /// What the user is asked (the request's asked permissions).
    asked: Vec<String>,
    /// The note that says it waits.
    seq: u64,
}

/// The page stopped at an untrusted certificate.
struct CertStop {
    url: String,
    host: String,
    code: i32,
    fingerprint: String,
    /// Tells this stop from an earlier one (`CertBlock::seq`).
    seq: u64,
}

/// A page's permission and certificate state.
pub(super) struct PageSites {
    queue: VecDeque<Waiting>,
    /// The bar's "Remember for this site".
    remember: bool,
    notes: VecDeque<PermissionNote>,
    /// The origin the page is at, for the answers it gave only for it.
    origin: Option<String>,
    /// Allowed or blocked for the page and origin only.
    once: HashMap<String, Setting>,
    /// Granted prompts Chromium remembers anyway, taken back when the page
    /// leaves the origin or closes: (origin, permission).
    temporary: Vec<(String, String)>,
    /// macOS refused what the user allowed: what the bar says.
    system_denied: Option<&'static str>,
    cert: Option<CertStop>,
    info_open: bool,
    info_anchor: AnchorSlot,
}

impl Default for PageSites {
    fn default() -> Self {
        PageSites {
            queue: VecDeque::new(),
            remember: true,
            notes: VecDeque::new(),
            origin: None,
            once: HashMap::new(),
            temporary: Vec::new(),
            system_denied: None,
            cert: None,
            info_open: false,
            info_anchor: AnchorSlot::default(),
        }
    }
}

impl PageSites {
    fn note(&mut self, permissions: &[impl AsRef<str>], origin: &str, outcome: Outcome) -> u64 {
        let seq = next_seq();
        self.notes.push_back(PermissionNote {
            seq,
            permissions: permissions.iter().map(|p| p.as_ref().to_owned()).collect(),
            origin: origin.to_owned(),
            outcome,
        });
        while self.notes.len() > NOTES {
            self.notes.pop_front();
        }
        seq
    }
}

/// Whether macOS must also consent to `name` (TCC).
fn needs_system(name: &str) -> bool {
    matches!(name, "camera" | "microphone")
}

impl BrowserPage {
    fn site_profile(&self) -> monocode_cef::Profile {
        self.host.options.borrow().profile.clone()
    }

    /// The user sees the page now (its tab is in front), under an overlay
    /// or not.
    fn seen_by_user(&self) -> bool {
        matches!(self.host.shown.get(), Placement::Shown | Placement::Covered)
    }

    /// `BrowserEvent::PermissionRequested`.
    pub(super) fn on_permission_request(
        &mut self,
        request: monocode_cef::PermissionRequest,
        cx: &mut Context<Self>,
    ) {
        let names: Vec<&str> = request.permissions.clone();
        let Some(origin) = sites::origin_of(&request.origin) else {
            request.answer(monocode_cef::PermissionAnswer::Dismiss);
            self.sites
                .note(&names, &request.origin, Outcome::DeniedOrigin);
            return;
        };
        let store = page_store(&self.profiles, &self.site_profile());
        let decision = {
            let store = store.borrow();
            let once = &self.sites.once;
            sites::decide(
                &names,
                |name| once.get(name).copied().or_else(|| store.get(&origin, name)),
                self.seen_by_user(),
                self.active_agent().is_some(),
            )
        };
        match decision {
            Decision::Answer {
                allow: true,
                outcome,
            } => {
                if outcome != Outcome::AllowedAutomatically {
                    self.sites.note(&names, &origin, outcome);
                }
                self.grant(request, origin, names, cx);
            }
            Decision::Answer { outcome, .. } => {
                request.answer(if outcome == Outcome::BlockedBySetting {
                    monocode_cef::PermissionAnswer::Block
                } else {
                    // Nothing for Chromium to remember: the user did not say.
                    monocode_cef::PermissionAnswer::Dismiss
                });
                self.sites.note(&names, &origin, outcome);
            }
            Decision::Ask(asked) => {
                let seq = self.sites.note(&asked, &origin, Outcome::WaitingForUser);
                self.sites.queue.push_back(Waiting {
                    request,
                    origin,
                    asked,
                    seq,
                });
            }
        }
        cx.notify();
    }

    /// Gives the page what it asked for, once macOS agrees to the camera
    /// and microphone.
    fn grant(
        &mut self,
        request: monocode_cef::PermissionRequest,
        origin: String,
        names: Vec<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let system: Vec<&'static str> = names.iter().copied().filter(|n| needs_system(n)).collect();
        if system.is_empty() {
            request.answer(monocode_cef::PermissionAnswer::Allow);
            return;
        }
        cx.spawn(async move |this, cx| {
            let mut refused = None;
            for name in system {
                if !tcc::consent(name).await {
                    refused = Some(name);
                    break;
                }
            }
            let _ = this.update(cx, |page, cx| match refused {
                None => request.answer(monocode_cef::PermissionAnswer::Allow),
                Some(name) => {
                    request.answer(monocode_cef::PermissionAnswer::Dismiss);
                    page.sites.system_denied = Some(if name == "camera" {
                        "camera"
                    } else {
                        "microphone"
                    });
                    page.sites.note(&names, &origin, Outcome::DeniedBySystem);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The bar's Allow or Block for the request in front.
    pub(super) fn answer_permission(&mut self, allow: bool, cx: &mut Context<Self>) {
        let Some(waiting) = self.sites.queue.pop_front() else {
            return;
        };
        let remember = std::mem::replace(&mut self.sites.remember, true);
        let setting = if allow {
            Setting::Allow
        } else {
            Setting::Block
        };
        let profile = self.site_profile();
        let store = page_store(&self.profiles, &profile);
        for name in &waiting.asked {
            if remember {
                store.borrow_mut().set(&waiting.origin, name, setting);
            } else {
                self.sites.once.insert(name.clone(), setting);
                // Chromium keeps a granted prompt for the site; it goes
                // when the page does. Media it never keeps.
                if allow && !needs_system(name) {
                    store.borrow_mut().note_temporary(&waiting.origin, name);
                    (self.sites.temporary).push((waiting.origin.clone(), name.clone()));
                }
            }
        }
        let outcome = if allow {
            Outcome::AllowedByUser
        } else {
            Outcome::BlockedByUser
        };
        self.sites.note(&waiting.asked, &waiting.origin, outcome);
        if allow {
            let names = waiting.request.permissions.clone();
            self.grant(waiting.request, waiting.origin, names, cx);
        } else {
            waiting.request.answer(if remember {
                monocode_cef::PermissionAnswer::Block
            } else {
                monocode_cef::PermissionAnswer::Dismiss
            });
        }
        cx.notify();
    }

    /// `BrowserEvent::PermissionGone`: the page navigated or closed.
    pub(super) fn permission_gone(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(ix) = self.sites.queue.iter().position(|w| w.request.id == id) else {
            return;
        };
        if let Some(waiting) = self.sites.queue.remove(ix) {
            self.sites
                .note(&waiting.asked, &waiting.origin, Outcome::Dismissed);
        }
        cx.notify();
    }

    /// The page is at `url` now: answers given for the earlier origin only
    /// end, and so does a certificate warning for another host.
    pub(super) fn sites_follow_address(&mut self, url: &str) {
        let origin = sites::origin_of(url);
        if origin != self.sites.origin {
            self.sites.once.clear();
            self.end_temporary(|o| Some(o) != origin.as_deref());
            // Media requests have no dismissal of their own.
            let left = std::mem::take(&mut self.sites.queue);
            for waiting in left {
                if Some(&waiting.origin) == origin.as_ref() {
                    self.sites.queue.push_back(waiting);
                } else {
                    self.sites
                        .note(&waiting.asked, &waiting.origin, Outcome::Dismissed);
                }
            }
            self.sites.origin = origin;
            self.sites.system_denied = None;
        }
        if (self.sites.cert.as_ref()).is_some_and(|stop| stop.host != sites::host_of(url)) {
            self.sites.cert = None;
        }
    }

    /// Takes back the page's temporary grants whose origin `ends`.
    fn end_temporary(&mut self, ends: impl Fn(&str) -> bool) {
        if self.sites.temporary.is_empty() {
            return;
        }
        let profile = self.site_profile();
        let store = store(&self.profiles, &profile);
        let (gone, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.sites.temporary)
            .into_iter()
            .partition(|(origin, _)| ends(origin));
        self.sites.temporary = kept;
        for (origin, name) in gone {
            store.borrow_mut().end_temporary(&origin, &name);
            monocode_cef::reset_site_permission(&profile, &origin, &name);
        }
    }

    /// The page goes: its temporary grants end, its prompts go unanswered.
    pub(super) fn sites_released(&mut self) {
        self.end_temporary(|_| true);
        self.sites.queue.clear();
    }

    /// `BrowserEvent::CertificateError`.
    pub(super) fn on_certificate_error(
        &mut self,
        request: monocode_cef::CertificateRequest,
        cx: &mut Context<Self>,
    ) {
        let host = sites::host_of(&request.url);
        let decision = EXCEPTIONS
            .with(|e| sites::decide_cert(&request.url, &request.fingerprint, &e.borrow()));
        match decision {
            CertDecision::ProceedLocal | CertDecision::ProceedException => {
                INSECURE.with(|i| i.borrow_mut().insert(host));
                request.proceed();
            }
            CertDecision::Block => {
                request.cancel();
                self.sites.cert = Some(CertStop {
                    url: request.url.clone(),
                    host,
                    code: request.code,
                    fingerprint: request.fingerprint.clone(),
                    seq: next_seq(),
                });
                self.failure = None;
            }
        }
        cx.notify();
    }

    /// Whether a load failure is the certificate warning's, which says it
    /// on its own.
    pub(super) fn failure_is_cert(&self, code: i32) -> bool {
        self.sites.cert.is_some() && (-299..=-200).contains(&code)
    }

    /// "Proceed (unsafe)": the host and certificate load until the app quits.
    fn proceed_past_cert(&mut self, cx: &mut Context<Self>) {
        let Some(stop) = self.sites.cert.take() else {
            return;
        };
        EXCEPTIONS.with(|e| e.borrow_mut().allow(&stop.host, &stop.fingerprint));
        self.host.with_view(|view| view.load_url(&stop.url));
        cx.notify();
    }

    /// "Back to safety".
    fn leave_cert(&mut self, cx: &mut Context<Self>) {
        self.sites.cert = None;
        let back = self.can_go_back;
        self.host.with_view(|view| {
            if back {
                view.go_back();
            } else {
                view.load_url("about:blank");
            }
        });
        cx.notify();
    }

    // ---- For the agent tools ----

    pub(crate) fn permission_notes(&self) -> Vec<PermissionNote> {
        self.sites.notes.iter().cloned().collect()
    }

    pub(crate) fn waiting_permission(&self) -> Option<PermissionNote> {
        let waiting = self.sites.queue.front()?;
        Some(PermissionNote {
            seq: waiting.seq,
            permissions: waiting.asked.clone(),
            origin: waiting.origin.clone(),
            outcome: Outcome::WaitingForUser,
        })
    }

    pub(crate) fn cert_block(&self) -> Option<CertBlock> {
        let stop = self.sites.cert.as_ref()?;
        Some(CertBlock {
            url: stop.url.clone(),
            host: stop.host.clone(),
            error: sites::cert_error_name(stop.code),
            seq: stop.seq,
        })
    }

    // ---- What the page shows ----

    /// The bar under the toolbar: the permission asked in front, or what
    /// macOS refused. It takes room from the page instead of covering it.
    pub(super) fn permission_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if let Some(waiting) = self.sites.queue.front() {
            return Some(self.ask_bar(waiting, cx));
        }
        let what = self.sites.system_denied?;
        let pane = if what == "camera" {
            "Privacy_Camera"
        } else {
            "Privacy_Microphone"
        };
        Some(
            bar()
                .child(icon("alert-circle", 14.).text_color(crate::modal::danger(1.)))
                .child(
                    div().flex_1().min_w_0().text_size(px(12.)).child(format!(
                        "macOS does not let Monocode use the {what}. Allow it in System Settings → Privacy & Security, then reload the page."
                    )),
                )
                .child(
                    modal::ghost_button("browser-permission-settings", "Open System Settings")
                        .h(px(26.))
                        .cursor_pointer()
                        .on_click(move |_, _, cx| {
                            cx.open_url(&format!(
                                "x-apple.systempreferences:com.apple.preference.security?{pane}"
                            ))
                        }),
                )
                .child(
                    icon_button("browser-permission-settings-close", "Close", "x").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.sites.system_denied = None;
                            cx.notify();
                        }),
                    ),
                )
                .into_any_element(),
        )
    }

    fn ask_bar(&self, waiting: &Waiting, cx: &mut Context<Self>) -> AnyElement {
        let site = sites::host_of(&waiting.origin);
        let what = (waiting.asked.iter())
            .map(|n| sites::label(n).to_lowercase())
            .collect::<Vec<_>>()
            .join(", ");
        let more = self.sites.queue.len() - 1;
        let remember = self.sites.remember;
        bar()
            .id("browser-permission-bar")
            .child(icon("square-lock", 14.).text_color(content(0.6)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.))
                    .child(format!("{site} wants to {what}."))
                    .when(more > 0, |d| {
                        d.child(
                            div()
                                .text_size(px(11.))
                                .text_color(content(0.5))
                                .child(format!("{more} more waiting")),
                        )
                    }),
            )
            .child(
                div()
                    .id("browser-permission-remember")
                    .role(gpui::Role::CheckBox)
                    .aria_label("Remember for this site")
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1()
                    .h(px(24.))
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(px(11.5))
                    .text_color(content(0.7))
                    .hover(|s| s.bg(content(0.06)))
                    .child(
                        div()
                            .size(px(13.))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(content(0.35))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(remember, |d| {
                                d.child(icon("check", 11.).text_color(content(0.9)))
                            }),
                    )
                    .child("Remember for this site")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.sites.remember = !this.sites.remember;
                        cx.notify();
                    })),
            )
            .child(
                modal::ghost_button("browser-permission-block", "Block")
                    .h(px(26.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.answer_permission(false, cx))),
            )
            .child(
                modal::primary_button("browser-permission-allow", "Allow")
                    .h(px(26.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.answer_permission(true, cx))),
            )
            .into_any_element()
    }

    /// The warning shown instead of a page whose certificate is not trusted.
    pub(super) fn cert_interstitial(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let stop = self.sites.cert.as_ref()?;
        let red = crate::modal::danger(1.);
        Some(
            div()
                .id("browser-cert-warning")
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(crate::modal::danger(0.08))
                .child(
                    div()
                        .max_w(px(520.))
                        .px_6()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(icon("shield-alert", 40.).text_color(red))
                        .child(
                            div()
                                .text_size(px(20.))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child("Your connection is not private"),
                        )
                        .child(div().text_size(px(13.)).text_color(content(0.75)).child(format!(
                            "Someone may be trying to steal your information from {} (passwords, messages or credit cards). {}",
                            stop.host,
                            sites::cert_error_text(stop.code)
                        )))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(content(0.5))
                                .child(sites::cert_error_name(stop.code)),
                        )
                        .child(
                            div()
                                .mt_2()
                                .flex()
                                .gap_2()
                                .child(
                                    modal::primary_button("browser-cert-back", "Back to safety")
                                        .cursor_pointer()
                                        .on_click(cx.listener(|this, _, _, cx| this.leave_cert(cx))),
                                )
                                .child(
                                    modal::danger_ghost_button(
                                        "browser-cert-proceed",
                                        format!("Proceed to {} (unsafe)", stop.host),
                                    )
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.proceed_past_cert(cx)),
                                    ),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The address bar's site button: a lock, or "Not secure".
    pub(super) fn site_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let url = &self.url;
        if !(url.starts_with("https:") || url.starts_with("http:")) {
            return None;
        }
        let unsafe_ = insecure(url) || self.sites.cert.is_some();
        let plain = url.starts_with("http:") && !sites::is_local_host(url);
        let toggle = cx.listener(|this, _, _, cx| {
            this.sites.info_open = !this.sites.info_open;
            cx.notify();
        });
        let button = if unsafe_ || plain {
            div()
                .id("browser-site-info")
                .role(gpui::Role::Button)
                .aria_label("Site information")
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .h(px(20.))
                .rounded_md()
                .cursor_pointer()
                .text_size(px(11.))
                .text_color(if unsafe_ {
                    crate::modal::danger(1.)
                } else {
                    content(0.6)
                })
                .hover(|s| s.bg(content(0.08)))
                .child(icon("alert-circle", 13.).text_color(if unsafe_ {
                    crate::modal::danger(1.)
                } else {
                    content(0.6)
                }))
                .child("Not secure")
                .on_click(toggle)
                .into_any_element()
        } else {
            icon_button("browser-site-info", "Site information", "square-lock")
                .on_click(toggle)
                .into_any_element()
        };
        Some(
            div()
                .relative()
                .flex_shrink_0()
                .child(self.sites.info_anchor.probe())
                .child(button)
                .into_any_element(),
        )
    }

    /// The site-info list: the connection, and the site's permissions with
    /// a reset each.
    pub(super) fn site_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.sites.info_open {
            return None;
        }
        let origin = sites::origin_of(&self.url)?;
        let profile = self.site_profile();
        let dir = profile_dir(&self.profiles, &profile);
        let remembered = store(&self.profiles, &profile).borrow().site(&origin);
        let connection = if insecure(&self.url) || self.sites.cert.is_some() {
            "Not secure: this site's certificate is not trusted. You chose to go on, or it is a local address; this lasts until Monocode quits."
        } else if self.url.starts_with("https:") {
            "Connection is secure."
        } else if sites::is_local_host(&self.url) {
            "A local address, without encryption."
        } else {
            "Not secure: information you send to this site can be read by others."
        };
        let weak = cx.weak_entity();
        let mut rows: Vec<AnyElement> = Vec::new();
        for (ix, (name, setting)) in remembered.iter().enumerate() {
            let (o, n, d) = (origin.clone(), name.clone(), dir.clone());
            let root = self.profiles.clone();
            let profile = profile.clone();
            rows.push(permission_row(
                ix,
                sites::title(name),
                match setting {
                    Setting::Allow => "Allowed",
                    Setting::Block => "Blocked",
                },
                move |cx: &mut App| {
                    match &d {
                        Some(dir) => forget(&root, dir, &o, &n),
                        None => {
                            store(&root, &profile).borrow_mut().remove(&o, &n);
                        }
                    }
                    cx.refresh_windows();
                },
            ));
        }
        for (ix, (name, setting)) in self.sites.once.iter().enumerate() {
            let n = name.clone();
            let weak = weak.clone();
            rows.push(permission_row(
                1000 + ix,
                sites::title(name),
                match setting {
                    Setting::Allow => "Allowed on this page",
                    Setting::Block => "Blocked on this page",
                },
                move |cx: &mut App| {
                    let _ = weak.update(cx, |page, cx| {
                        page.sites.once.remove(&n);
                        cx.notify();
                    });
                },
            ));
        }
        let empty = rows.is_empty();
        let dismiss = weak.clone();
        Some(
            popover(
                "browser-site-popover",
                Anchor::Slot(self.sites.info_anchor.clone()),
            )
            .align(Align::Start)
            .width(320.)
            .max_height(360.)
            .padding(10.)
            .on_dismiss(move |_, cx| {
                let _ = dismiss.update(cx, |this, cx| {
                    this.sites.info_open = false;
                    cx.notify();
                });
            })
            .child(
                div()
                    .id("browser-site-info-list")
                    .flex()
                    .flex_col()
                    .gap_2()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(sites::host_of(&origin)),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(content(0.6))
                            .child(connection),
                    )
                    .child(div().h(px(1.)).bg(content(0.08)))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(content(0.5))
                            .child("Permissions"),
                    )
                    .when(empty, |d| {
                        d.child(
                            div()
                                .text_size(px(12.))
                                .text_color(content(0.6))
                                .child("This site has not asked for any yet."),
                        )
                    })
                    .children(rows),
            )
            .into_any_element(),
        )
    }
}

/// The bar's frame (also the passkey bar's).
pub(super) fn bar() -> gpui::Stateful<gpui::Div> {
    div()
        .id("browser-site-bar")
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .py_1()
        .min_h(px(36.))
        .flex_shrink_0()
        .border_b_1()
        .border_color(content(0.08))
        .bg(content(0.03))
}

/// One permission in a list: its name, its answer, and a reset.
pub(crate) fn permission_row(
    ix: usize,
    title: &'static str,
    state: &'static str,
    reset: impl Fn(&mut App) + 'static,
) -> AnyElement {
    div()
        .id(("browser-site-permission", ix))
        .flex()
        .items_center()
        .gap_2()
        .child(div().flex_1().text_size(px(12.5)).child(title))
        .child(
            div()
                .text_size(px(11.5))
                .text_color(content(0.55))
                .child(state),
        )
        .child(
            modal::ghost_button(("browser-site-permission-reset", ix), "Reset")
                .h(px(24.))
                .px_2()
                .text_size(px(11.5))
                .cursor_pointer()
                .on_click(move |_, _, cx| reset(cx)),
        )
        .into_any_element()
}

/// macOS's own consent to the camera and microphone (TCC).
mod tcc {
    /// Whether macOS lets the app use `name` ("camera" or "microphone"),
    /// asking the user the first time.
    #[cfg(target_os = "macos")]
    pub(super) async fn consent(name: &str) -> bool {
        use objc2::{msg_send, runtime::AnyClass};
        use objc2_foundation::{NSBundle, NSString};

        let (media, usage) = if name == "camera" {
            ("vide", "NSCameraUsageDescription")
        } else {
            ("soun", "NSMicrophoneUsageDescription")
        };
        // Without a usage string (a binary run outside its bundle) macOS
        // kills the app on asking: Chromium is left to fail on its own.
        let described = NSBundle::mainBundle()
            .objectForInfoDictionaryKey(&NSString::from_str(usage))
            .is_some();
        if !described {
            return true;
        }
        let Some(class) = AnyClass::get(c"AVCaptureDevice") else {
            return true;
        };
        let media = NSString::from_str(media);
        // AVAuthorizationStatus: 0 not determined, 1 restricted, 2 denied,
        // 3 authorized.
        let status: isize = unsafe { msg_send![class, authorizationStatusForMediaType: &*media] };
        match status {
            3 => return true,
            0 => {}
            _ => return false,
        }
        let (tx, rx) = async_channel::bounded::<bool>(1);
        let block = block2::RcBlock::new(move |granted: objc2::runtime::Bool| {
            let _ = tx.try_send(granted.as_bool());
        });
        let _: () = unsafe {
            msg_send![class, requestAccessForMediaType: &*media, completionHandler: &*block]
        };
        rx.recv().await.unwrap_or(false)
    }

    #[cfg(not(target_os = "macos"))]
    pub(super) async fn consent(_: &str) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_cef::{CertificateRequest, PermissionRequest};

    fn page_in(root: &Path, url: &str, cx: &mut gpui::TestAppContext) -> Entity<BrowserPage> {
        let (url, root) = (url.to_owned(), root.to_path_buf());
        cx.new(|cx| {
            let mut page = BrowserPage::new(url.clone(), String::new(), 100, root, cx);
            page.sites_follow_address(&url);
            page
        })
    }

    fn outcomes(page: &BrowserPage) -> Vec<&'static str> {
        (page.permission_notes().iter())
            .map(|n| n.outcome.as_str())
            .collect()
    }

    #[gpui::test]
    fn the_user_is_asked_once_and_the_answer_stays_with_the_profile(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let page = page_in(root.path(), "https://maps.test/", cx);
        page.update(cx, |page, cx| {
            page.host.shown.set(Placement::Shown);
            let ask = PermissionRequest::detached(1, "https://maps.test/", vec!["geolocation"]);
            page.on_permission_request(ask, cx);
            let waiting = page.waiting_permission().expect("asked");
            assert_eq!(waiting.permissions, ["geolocation"]);
            assert_eq!(outcomes(page), ["waiting_for_user"]);
            page.answer_permission(true, cx);
            assert!(page.waiting_permission().is_none());
            // Asked again: answered from the setting.
            let again = PermissionRequest::detached(2, "https://maps.test/", vec!["geolocation"]);
            page.on_permission_request(again, cx);
            assert!(page.waiting_permission().is_none());
            assert_eq!(
                outcomes(page),
                ["waiting_for_user", "allowed_by_user", "allowed_by_setting"]
            );
        });
        // Saved in the default profile's folder, and listed for Settings.
        let saved = SitePermissions::load(&root.path().join("Profiles/default"));
        assert_eq!(
            saved.get("https://maps.test", "geolocation"),
            Some(Setting::Allow)
        );
        let listed = all_sites(root.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1[0].0, "https://maps.test");
        forget(
            root.path(),
            &root.path().join("Profiles/default"),
            "https://maps.test",
            "geolocation",
        );
        assert!(all_sites(root.path()).is_empty());
    }

    #[gpui::test]
    fn an_agents_background_page_is_refused_and_told(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let page = page_in(root.path(), "https://meet.test/", cx);
        page.update(cx, |page, cx| {
            page.agent_used(7, "Agent".into(), cx);
            assert_eq!(page.host.shown.get(), Placement::Hidden);
            let ask =
                PermissionRequest::detached(1, "https://meet.test", vec!["camera", "microphone"]);
            page.on_permission_request(ask, cx);
            assert!(page.waiting_permission().is_none());
            let note = page.permission_notes().pop().unwrap();
            assert_eq!(note.outcome, Outcome::DeniedBackground);
            assert_eq!(note.permissions, ["camera", "microphone"]);
            // Unsupported ones are refused for anyone; harmless ones granted.
            let share = PermissionRequest::detached(2, "https://meet.test", vec!["screen_share"]);
            page.on_permission_request(share, cx);
            let lock = PermissionRequest::detached(3, "https://meet.test", vec!["pointer_lock"]);
            page.on_permission_request(lock, cx);
            assert_eq!(outcomes(page), ["denied_background", "denied_unsupported"]);
        });
        // Nothing was remembered.
        assert!(all_sites(root.path()).is_empty());
    }

    #[gpui::test]
    fn a_users_background_tab_waits_and_requests_queue(cx: &mut gpui::TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let page = page_in(root.path(), "https://a.test/", cx);
        page.update(cx, |page, cx| {
            for (id, what) in [(1, "notifications"), (2, "clipboard_read")] {
                let ask = PermissionRequest::detached(id, "https://a.test", vec![what]);
                page.on_permission_request(ask, cx);
            }
            assert_eq!(page.sites.queue.len(), 2);
            // Not remembered: blocked for the page only.
            page.sites.remember = false;
            page.answer_permission(false, cx);
            assert_eq!(page.sites.once.get("notifications"), Some(&Setting::Block));
            assert!(page.sites.remember, "the next one remembers again");
            assert_eq!(
                page.waiting_permission().unwrap().permissions,
                ["clipboard_read"]
            );
            // Chromium voids the prompt when the page navigates.
            page.permission_gone(2, cx);
            assert!(page.waiting_permission().is_none());
            page.sites_follow_address("https://b.test/");
            assert!(page.sites.once.is_empty());
            // An opaque origin cannot hold a permission.
            let file = PermissionRequest::detached(3, "file:///x", vec!["camera"]);
            page.on_permission_request(file, cx);
            assert_eq!(
                page.permission_notes().pop().unwrap().outcome,
                Outcome::DeniedOrigin
            );
        });
        assert!(all_sites(root.path()).is_empty());
    }

    #[gpui::test]
    fn certificates_of_local_hosts_pass_and_others_stop_until_proceeded(
        cx: &mut gpui::TestAppContext,
    ) {
        let root = tempfile::tempdir().unwrap();
        let page = page_in(root.path(), "about:blank", cx);
        page.update(cx, |page, cx| {
            let local = CertificateRequest::detached("https://192.168.1.5:8443/", -202, "aa");
            page.on_certificate_error(local, cx);
            assert!(page.cert_block().is_none());
            assert!(insecure("https://192.168.1.5:8443/x"));

            let public = CertificateRequest::detached("https://self-signed.test/", -202, "bb");
            page.on_certificate_error(public, cx);
            let block = page.cert_block().unwrap();
            assert_eq!(block.host, "self-signed.test");
            assert_eq!(block.error, "ERR_CERT_AUTHORITY_INVALID");
            assert!(page.failure_is_cert(-202));
            assert!(!insecure("https://self-signed.test/"));

            page.proceed_past_cert(cx);
            assert!(page.cert_block().is_none());
            let again = CertificateRequest::detached("https://self-signed.test/", -202, "bb");
            page.on_certificate_error(again, cx);
            assert!(page.cert_block().is_none(), "proceeded for this run");
            assert!(insecure("https://self-signed.test/"));
            // Another certificate of the host asks again.
            let other = CertificateRequest::detached("https://self-signed.test/", -202, "cc");
            page.on_certificate_error(other, cx);
            assert!(page.cert_block().is_some());
            // Leaving for another host ends the warning.
            page.sites_follow_address("https://elsewhere.test/");
            assert!(page.cert_block().is_none());
        });
    }
}
