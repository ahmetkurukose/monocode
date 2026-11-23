//! Saved passwords in a page (docs/native/browser/context.md K56, K57,
//! K60). The watcher (`monocode_core::browser::passwords::watch`) runs in
//! an isolated world of every document and reports here:
//!
//! - A sign-in that worked ([`capture`]) is offered for saving in a bar
//!   under the toolbar (Save / Never for this site / ×), which takes room
//!   from the page instead of covering it (K44); a changed password is
//!   offered as an update. Dismissed, the offer stays under the address
//!   bar's key for [`OFFER_KEPT`].
//! - When a sign-in field gets the focus on a site with saved logins, the
//!   toolbar offers to fill them ("Fill as …"): over the page a dropdown
//!   would make the page a still picture while it shows (K44). The key in
//!   the address bar lists the site's logins to fill too.
//! - Agents fill saved logins through `browser_fill_login` and never see
//!   them: [`BrowserPage::agent_secrets`] is what their answers mask.
//!
//! Throwaway profiles (K22) neither save nor fill.
use super::*;
use monocode_core::browser::passwords::{
    Offer,
    capture::{Tracker, Verdict},
    origin_of,
    watch::{FillTarget, Report, Signal},
};

use crate::browser::passwords::{self as vaults, VaultState};

/// A dismissed offer stays reachable under the key this long (Chrome's).
const OFFER_KEPT: Duration = Duration::from_secs(5 * 60);
/// Passwords the user signed in with on a page, kept for masking.
const SEEN: usize = 20;

/// "Save password?" for a sign-in that worked.
#[derive(Clone, Debug)]
struct SaveOffer {
    store: String,
    url: String,
    origin: String,
    username: String,
    password: String,
    /// The saved login whose password changed.
    update: Option<i64>,
    at: Instant,
    /// The bar shows it (not dismissed).
    shown: bool,
    /// Waiting for the vault to open to act on it.
    busy: bool,
    /// What went wrong acting on it.
    error: Option<String>,
}

/// The sign-in field that last had the focus in the page's document.
#[derive(Clone, Debug)]
struct Focused {
    context: i64,
    field: String,
}

#[derive(Default)]
pub(super) struct PagePasswords {
    tracker: Tracker,
    focus: Option<Focused>,
    offer: Option<SaveOffer>,
    seen: Vec<String>,
    ticking: Option<gpui::Task<()>>,
    key_open: bool,
    key_anchor: AnchorSlot,
    /// A fill on its way, or why the last one failed.
    filling: bool,
    fill_error: Option<String>,
}

impl Host<monocode_cef::BrowserView> {
    /// Starts the password watcher on a new view's channel, unless the
    /// page browses with a throwaway profile.
    pub(super) fn watch_passwords(
        &self,
        cdp: &Rc<CdpClient>,
    ) -> Option<monocode_core::browser::passwords::watch::PasswordWatch> {
        if vaults::store_name(&self.options.borrow().profile).is_none() {
            return None;
        }
        let shared = Rc::downgrade(&self.shared);
        Some(monocode_core::browser::passwords::watch::watch(
            cdp,
            move |report| {
                if let Some(shared) = shared.upgrade()
                    && let Some(sender) = shared.passwords.borrow().as_ref()
                {
                    let _ = sender.try_send(report);
                }
            },
        ))
    }
}

impl BrowserPage {
    /// Hears the password watcher's reports from any of the page's views.
    pub(super) fn listen_passwords(self, cx: &mut Context<Self>) -> Self {
        let (sender, reports) = async_channel::unbounded::<Report>();
        *self.host.shared.passwords.borrow_mut() = Some(sender);
        cx.spawn(async move |this, cx| {
            while let Ok(report) = reports.recv().await {
                if this
                    .update(cx, |page, cx| page.on_password_report(report, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        self
    }

    /// The vault this page's profile uses, if any.
    fn password_store(&self) -> Option<String> {
        vaults::store_name(&self.host.options.borrow().profile)
    }

    fn on_password_report(&mut self, report: Report, cx: &mut Context<Self>) {
        let Report { context, signal } = report;
        let now = monocode_core::time::now_ms();
        let verdict = match signal {
            Signal::Focus { field, .. } => {
                if vaults::password_manager_enabled(cx) && self.password_store().is_some() {
                    self.passwords.focus = Some(Focused { context, field });
                    self.passwords.fill_error = None;
                    cx.notify();
                }
                None
            }
            // The field keeps its offer: a click on it in the toolbar
            // takes the focus first.
            Signal::Blur { .. } => None,
            Signal::Submit {
                url,
                username,
                password,
            } => {
                self.remember_seen(&password);
                (self.passwords.tracker).submitted(&url, &username, &password, context, now);
                self.tick_passwords(cx);
                None
            }
            Signal::Username { url, username } => {
                self.passwords.tracker.username_step(&url, &username, now);
                None
            }
            Signal::Gone => self.passwords.tracker.gone(context),
            Signal::Forms { password } => self.passwords.tracker.forms(context, password, now),
        };
        if let Some(Verdict::Success(submission)) = verdict {
            self.offer_to_save(submission, cx);
        }
    }

    fn remember_seen(&mut self, password: &str) {
        let seen = &mut self.passwords.seen;
        if !password.is_empty() && !seen.iter().any(|p| p == password) {
            seen.push(password.to_owned());
            if seen.len() > SEEN {
                seen.remove(0);
            }
        }
    }

    /// Asks the tracker what the time decides, while a sign-in waits.
    fn tick_passwords(&mut self, cx: &mut Context<Self>) {
        if self.passwords.ticking.is_some() {
            return;
        }
        self.passwords.ticking = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let waiting = this.update(cx, |page, cx| {
                    let now = monocode_core::time::now_ms();
                    if let Some(Verdict::Success(submission)) = page.passwords.tracker.tick(now) {
                        page.offer_to_save(submission, cx);
                    }
                    let waiting = page.passwords.tracker.waiting();
                    if !waiting {
                        page.passwords.ticking = None;
                    }
                    waiting
                });
                if !matches!(waiting, Ok(true)) {
                    return;
                }
            }
        }));
    }

    /// A sign-in worked: offer to save it, or to update the saved login.
    fn offer_to_save(
        &mut self,
        submission: monocode_core::browser::passwords::capture::Submission,
        cx: &mut Context<Self>,
    ) {
        if !vaults::password_manager_enabled(cx) {
            return;
        }
        let Some(store) = self.password_store() else {
            return;
        };
        let offer = vaults::with_vault(&store, cx, |vault| {
            vault.offer(&submission.url, &submission.username, &submission.password)
        });
        let update = match offer {
            Some(Ok(Offer::Nothing)) => return,
            Some(Ok(Offer::Update { id, .. })) => Some(id),
            Some(Ok(Offer::Save)) => None,
            Some(Err(error)) => {
                log::warn!("browser passwords: {error}");
                return;
            }
            // Locked with passwords saved: open it, then decide (the site
            // may be saved already, or never to be).
            None if vaults::saved_any(cx) && vaults::state(cx) != VaultState::Open => {
                if let VaultState::Unavailable(why) = vaults::state(cx) {
                    // The Keychain said no: the bar says so, Save tries again.
                    self.show_offer(store, submission, None, Some(why), cx);
                    return;
                }
                let unlocking = vaults::unlock(cx);
                cx.spawn(async move |this, cx| {
                    let state = unlocking.await;
                    let _ = this.update(cx, |page, cx| match state {
                        VaultState::Open => page.offer_to_save(submission, cx),
                        VaultState::Unavailable(why) => {
                            page.show_offer(store, submission, None, Some(why), cx)
                        }
                        _ => {}
                    });
                })
                .detach();
                return;
            }
            // Nothing saved yet: the vault opens on Save.
            None => None,
        };
        self.show_offer(store, submission, update, None, cx);
    }

    fn show_offer(
        &mut self,
        store: String,
        submission: monocode_core::browser::passwords::capture::Submission,
        update: Option<i64>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.passwords.offer = Some(SaveOffer {
            store,
            url: submission.url,
            origin: submission.origin,
            username: submission.username,
            password: submission.password,
            update,
            at: Instant::now(),
            shown: true,
            busy: false,
            error,
        });
        cx.notify();
    }

    /// The page is at `url` now: a sign-in field of the old document has
    /// no focus to fill any more.
    pub(super) fn passwords_follow_address(&mut self, url: &str) {
        let _ = url;
        self.passwords.focus = None;
        self.passwords.fill_error = None;
        if (self.passwords.offer.as_ref()).is_some_and(|o| o.at.elapsed() > OFFER_KEPT) {
            self.passwords.offer = None;
        }
    }

    /// The page goes: nothing waits on its documents.
    pub(super) fn passwords_released(&mut self) {
        self.passwords.tracker.clear();
        self.passwords.ticking = None;
    }

    // ---- Acting on the offer ----

    /// Opens the vault (the Keychain may ask), then runs `act` on the
    /// offer's vault.
    fn with_open_vault(
        &mut self,
        act: impl FnOnce(&mut BrowserPage, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        if vaults::state(cx) == VaultState::Open {
            act(self, cx);
            return;
        }
        if let Some(offer) = &mut self.passwords.offer {
            offer.busy = true;
            offer.error = None;
        }
        let unlocking = vaults::unlock(cx);
        cx.spawn(async move |this, cx| {
            let state = unlocking.await;
            let _ = this.update(cx, |page, cx| {
                if let Some(offer) = &mut page.passwords.offer {
                    offer.busy = false;
                }
                match state {
                    VaultState::Open => act(page, cx),
                    VaultState::Unavailable(why) => {
                        if let Some(offer) = &mut page.passwords.offer {
                            offer.error = Some(why);
                        }
                    }
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The bar's "Save" (or "Update").
    fn save_offered(&mut self, cx: &mut Context<Self>) {
        self.with_open_vault(
            |page, cx| {
                let Some(offer) = page.passwords.offer.clone() else {
                    return;
                };
                let now = monocode_core::time::now_ms() as i64;
                let saved = vaults::with_vault(&offer.store, cx, |vault| {
                    // Decided again: the vault may have been locked at the offer.
                    let id = match offer.update {
                        Some(id) => {
                            let login = vault.get(id)?.ok_or("the login is gone")?;
                            vault.edit(id, &login.username, &offer.password, now)?;
                            id
                        }
                        None => vault.save(&offer.url, &offer.username, &offer.password, now)?,
                    };
                    vault.note_used(id, now)
                });
                match saved {
                    Some(Ok(())) => {
                        page.passwords.offer = None;
                        vaults::changed(cx);
                    }
                    Some(Err(error)) => {
                        if let Some(o) = &mut page.passwords.offer {
                            o.error = Some(error);
                        }
                    }
                    None => {}
                }
                cx.notify();
            },
            cx,
        );
    }

    /// The bar's "Never for this site".
    fn never_offered(&mut self, cx: &mut Context<Self>) {
        self.with_open_vault(
            |page, cx| {
                let Some(offer) = page.passwords.offer.take() else {
                    return;
                };
                let now = monocode_core::time::now_ms() as i64;
                if let Some(Err(error)) =
                    vaults::with_vault(&offer.store, cx, |vault| vault.never_save(&offer.url, now))
                {
                    log::warn!("browser passwords: {error}");
                }
                vaults::changed(cx);
                cx.notify();
            },
            cx,
        );
    }

    fn dismiss_offer(&mut self, cx: &mut Context<Self>) {
        if let Some(offer) = &mut self.passwords.offer {
            offer.shown = false;
        }
        cx.notify();
    }

    // ---- Filling ----

    /// The saved logins of the page's site (no passwords), if the vault is
    /// open and the password manager on.
    fn site_logins(&self, cx: &mut App) -> Vec<monocode_core::browser::passwords::Login> {
        let (Some(store), Some(origin)) = (self.password_store(), origin_of(&self.url)) else {
            return Vec::new();
        };
        if !vaults::password_manager_enabled(cx) {
            return Vec::new();
        }
        vaults::with_vault(&store, cx, |vault| vault.logins(Some(&origin)))
            .and_then(Result::ok)
            .unwrap_or_default()
    }

    /// Fills saved login `id` into the field that had the focus, or the
    /// page's first sign-in form.
    fn fill_saved(&mut self, id: i64, cx: &mut Context<Self>) {
        let Some(store) = self.password_store() else {
            return;
        };
        let Some(cdp) = self.cdp() else { return };
        let credential = vaults::with_vault(&store, cx, |vault| {
            let login = vault.get(id)?.ok_or("the login is gone")?;
            Ok((login.username, vault.password(id)?))
        });
        let Some(Ok((username, password))) = credential else {
            return;
        };
        let target = match &self.passwords.focus {
            Some(focus) => FillTarget::Field {
                context: focus.context,
                field: focus.field.clone(),
            },
            None => FillTarget::Page,
        };
        self.passwords.filling = true;
        self.passwords.key_open = false;
        cx.notify();
        cx.spawn(async move |this, cx| {
            use monocode_core::browser::passwords::watch::fill;
            let mut result = fill(&cdp, &target, &username, &password).await;
            // The focused field's document went: the page's form, then.
            if result.is_err() && target != FillTarget::Page {
                result = fill(&cdp, &FillTarget::Page, &username, &password).await;
            }
            let _ = this.update(cx, |page, cx| {
                page.passwords.filling = false;
                match result {
                    Ok(_) => {
                        page.passwords.fill_error = None;
                        let now = monocode_core::time::now_ms() as i64;
                        let _ = vaults::with_vault(&store, cx, |vault| vault.note_used(id, now));
                    }
                    Err(why) => page.passwords.fill_error = Some(why),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- For the agent tools (K57) ----

    /// The site's saved logins with their passwords, for
    /// `browser_fill_login`.
    pub(crate) fn agent_logins(
        &self,
        cx: &mut App,
    ) -> Result<Vec<monocode_core::browser::passwords::Credential>, String> {
        let Some(store) = self.password_store() else {
            return Err(
                "this page browses with a throwaway profile, which keeps no passwords".into(),
            );
        };
        if !vaults::password_manager_enabled(cx) {
            return Err("the user turned Monocode's password manager off".into());
        }
        let Some(origin) = origin_of(&self.url) else {
            return Ok(Vec::new());
        };
        match vaults::state(cx) {
            VaultState::Open => {}
            VaultState::Unavailable(_) => {
                return Err("the saved passwords are locked: the Keychain did not give their key; the user can unlock them in Settings → Passwords".into());
            }
            VaultState::Locked | VaultState::Unlocking => {
                if !vaults::saved_any(cx) {
                    return Ok(Vec::new());
                }
                vaults::unlock(cx).detach();
                return Err("the saved passwords are being unlocked (the user may need to allow it); try again in a moment".into());
            }
        }
        vaults::with_vault(&store, cx, |vault| vault.credentials(&origin))
            .unwrap_or_else(|| Ok(Vec::new()))
            .map(|found| {
                found
                    .into_iter()
                    .map(
                        |(login, password)| monocode_core::browser::passwords::Credential {
                            username: login.username,
                            password,
                        },
                    )
                    .collect()
            })
    }

    /// An agent filled `username`'s saved login.
    pub(crate) fn agent_login_used(&self, username: &str, cx: &mut App) {
        let (Some(store), Some(origin)) = (self.password_store(), origin_of(&self.url)) else {
            return;
        };
        let now = monocode_core::time::now_ms() as i64;
        let _ = vaults::with_vault(&store, cx, |vault| {
            match vault
                .logins(Some(&origin))?
                .iter()
                .find(|l| l.username == username)
            {
                Some(login) => vault.note_used(login.id, now),
                None => Ok(()),
            }
        });
    }

    /// What no agent may read on this page: the site's saved passwords
    /// (when the vault is open), and those the user signed in with here.
    pub(crate) fn agent_secrets(&self, cx: &mut App) -> Vec<String> {
        let mut secrets = self.passwords.seen.clone();
        if let Some(offer) = &self.passwords.offer {
            secrets.push(offer.password.clone());
        }
        if let (Some(store), Some(origin)) = (self.password_store(), origin_of(&self.url))
            && let Some(Ok(found)) =
                vaults::with_vault(&store, cx, |vault| vault.credentials(&origin))
        {
            secrets.extend(found.into_iter().map(|(_, password)| password));
        }
        secrets.sort();
        secrets.dedup();
        secrets
    }

    // ---- What the page shows ----

    /// "Save password?" under the toolbar.
    pub(super) fn password_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let offer = self.passwords.offer.as_ref().filter(|o| o.shown)?;
        let site = monocode_core::browser::sites::host_of(&offer.origin);
        let user = if offer.username.is_empty() {
            String::new()
        } else {
            format!(" for {}", offer.username)
        };
        let question = if offer.update.is_some() {
            format!("Update the saved password{user} on {site}?")
        } else {
            format!("Save the password{user} on {site}?")
        };
        let busy = offer.busy;
        let error = offer.error.clone();
        Some(
            div()
                .id("browser-password-bar")
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
                .child(icon("key", 14.).text_color(content(0.6)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.))
                        .child(if busy {
                            "Unlocking your saved passwords…".to_owned()
                        } else {
                            question
                        })
                        .when_some(error, |d, error| {
                            d.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(crate::modal::danger(1.))
                                    .child(error),
                            )
                        }),
                )
                .when(offer.update.is_none(), |d| {
                    d.child(
                        modal::ghost_button("browser-password-never", "Never for this site")
                            .h(px(26.))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.never_offered(cx))),
                    )
                })
                .child(
                    modal::primary_button(
                        "browser-password-save",
                        if offer.update.is_some() {
                            "Update"
                        } else {
                            "Save"
                        },
                    )
                    .h(px(26.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.save_offered(cx))),
                )
                .child(
                    icon_button("browser-password-dismiss", "Not now", "x")
                        .on_click(cx.listener(|this, _, _, cx| this.dismiss_offer(cx))),
                )
                .into_any_element(),
        )
    }

    /// The key at the end of the address bar: the site has saved logins,
    /// or an offer waits.
    pub(super) fn password_key_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.password_store().is_none() || !vaults::password_manager_enabled(cx) {
            return None;
        }
        let offer = self.passwords.offer.is_some();
        if !offer && self.site_logins(cx).is_empty() {
            return None;
        }
        Some(
            div()
                .relative()
                .flex_shrink_0()
                .child(self.passwords.key_anchor.probe())
                .child(
                    icon_button("browser-passwords-key", "Saved passwords", "key")
                        .when(self.passwords.key_open, |b| b.bg(content(0.1)))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.passwords.key_open = !this.passwords.key_open;
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }

    /// "Fill as …" in the toolbar while a sign-in field of a site with
    /// saved logins has the focus.
    pub(super) fn password_fill_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.passwords.focus.as_ref()?;
        let logins = self.site_logins(cx);
        let first = logins.first()?;
        let label = if self.passwords.filling {
            "Filling…".to_owned()
        } else if logins.len() == 1 {
            format!("Fill as {}", short(&first.username))
        } else {
            "Fill a saved login".to_owned()
        };
        let (id, single) = (first.id, logins.len() == 1);
        Some(
            div()
                .id("browser-password-fill")
                .role(gpui::Role::Button)
                .aria_label(label.clone())
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .h(px(24.))
                .max_w(px(220.))
                .rounded_md()
                .cursor_pointer()
                .text_size(px(11.5))
                .bg(crate::theme::accent_color().opacity(0.15))
                .hover(|s| s.bg(crate::theme::accent_color().opacity(0.25)))
                .child(icon("key", 13.).text_color(content(0.8)))
                .child(div().truncate().child(label))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if single {
                        this.fill_saved(id, cx);
                    } else {
                        this.passwords.key_open = true;
                        cx.notify();
                    }
                }))
                .into_any_element(),
        )
    }

    /// The key's list: the waiting offer, the site's logins to fill, and
    /// the way to Settings.
    pub(super) fn passwords_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.passwords.key_open {
            return None;
        }
        let origin = origin_of(&self.url)?;
        let logins = self.site_logins(cx);
        let state = vaults::state(cx);
        let weak = cx.weak_entity();
        let mut rows: Vec<AnyElement> = Vec::new();
        if let Some(offer) = &self.passwords.offer {
            let what = if offer.update.is_some() {
                "Update"
            } else {
                "Save"
            };
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.))
                            .truncate()
                            .child(format!(
                                "{what} the password for {}?",
                                if offer.username.is_empty() {
                                    "this site"
                                } else {
                                    &offer.username
                                }
                            )),
                    )
                    .child(
                        modal::primary_button("browser-passwords-key-save", what)
                            .h(px(24.))
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| this.save_offered(cx))),
                    )
                    .into_any_element(),
            );
            rows.push(div().h(px(1.)).bg(content(0.08)).into_any_element());
        }
        if let VaultState::Unavailable(why) = &state {
            rows.push(
                div()
                    .text_size(px(11.5))
                    .text_color(crate::modal::danger(1.))
                    .child(why.clone())
                    .into_any_element(),
            );
        }
        for (ix, login) in logins.iter().enumerate() {
            let id = login.id;
            rows.push(
                div()
                    .id(("browser-passwords-login", ix))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_1()
                    .h(px(28.))
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|s| s.bg(content(0.06)))
                    .child(icon("account-user", 13.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .child(if login.username.is_empty() {
                                "(no user name)".to_owned()
                            } else {
                                login.username.clone()
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(content(0.5))
                            .child("Fill"),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.fill_saved(id, cx)))
                    .into_any_element(),
            );
        }
        if let Some(error) = &self.passwords.fill_error {
            rows.push(
                div()
                    .text_size(px(11.))
                    .text_color(content(0.55))
                    .child(format!("Could not fill: {error}"))
                    .into_any_element(),
            );
        }
        let dismiss = weak.clone();
        Some(
            popover(
                "browser-passwords-popover",
                Anchor::Slot(self.passwords.key_anchor.clone()),
            )
            .align(Align::End)
            .width(300.)
            .max_height(360.)
            .padding(10.)
            .on_dismiss(move |_, cx| {
                let _ = dismiss.update(cx, |this, cx| {
                    this.passwords.key_open = false;
                    cx.notify();
                });
            })
            .child(
                div()
                    .id("browser-passwords-list")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!(
                                "Passwords for {}",
                                monocode_core::browser::sites::host_of(&origin)
                            )),
                    )
                    .children(rows)
                    .child(div().h(px(1.)).bg(content(0.08)))
                    .child(
                        div()
                            .id("browser-passwords-manage")
                            .px_1()
                            .h(px(26.))
                            .flex()
                            .items_center()
                            .rounded_md()
                            .cursor_pointer()
                            .text_size(px(12.))
                            .text_color(content(0.7))
                            .hover(|s| s.bg(content(0.06)))
                            .child("Manage passwords…")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.passwords.key_open = false;
                                window.dispatch_action(Box::new(ShowBrowserPasswords), cx);
                                cx.notify();
                            })),
                    ),
            )
            .into_any_element(),
        )
    }

    // ---- Tests ----

    #[cfg(test)]
    pub(crate) fn password_report_for_test(&mut self, report: Report, cx: &mut Context<Self>) {
        self.on_password_report(report, cx);
    }

    #[cfg(test)]
    pub(crate) fn password_offer_for_test(&self) -> Option<(String, String, bool, bool)> {
        let o = self.passwords.offer.as_ref()?;
        Some((
            o.origin.clone(),
            o.username.clone(),
            o.update.is_some(),
            o.shown,
        ))
    }
}

actions!(
    workspace,
    [
        /// Settings → Passwords.
        ShowBrowserPasswords
    ]
);

fn short(text: &str) -> String {
    if text.chars().count() <= 28 {
        return text.to_owned();
    }
    let cut: String = text.chars().take(27).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(url: &str, cx: &mut gpui::TestAppContext) -> Entity<BrowserPage> {
        cx.new(|cx| BrowserPage::new(url.into(), String::new(), 100, "/tmp".into(), cx))
    }

    fn report(context: i64, signal: Signal) -> Report {
        Report { context, signal }
    }

    fn submit(user: &str, password: &str) -> Signal {
        Signal::Submit {
            url: "https://a.test/login".into(),
            username: user.into(),
            password: password.into(),
        }
    }

    #[gpui::test]
    fn a_sign_in_that_worked_is_offered_and_saved(cx: &mut gpui::TestAppContext) {
        cx.update(vaults::open_for_test);
        let page = page("https://a.test/login", cx);
        page.update(cx, |page, cx| {
            page.password_report_for_test(report(1, Signal::Forms { password: true }), cx);
            page.password_report_for_test(report(1, submit("alice", "pw-1234")), cx);
            assert_eq!(page.password_offer_for_test(), None);
            // The single-page app moved on: the form went away.
            page.password_report_for_test(report(1, Signal::Gone), cx);
            assert_eq!(
                page.password_offer_for_test(),
                Some(("https://a.test".into(), "alice".into(), false, true))
            );
            // Agents never read what the user signed in with.
            assert!(page.agent_secrets(cx).contains(&"pw-1234".to_owned()));
            page.save_offered(cx);
            assert_eq!(page.password_offer_for_test(), None);
            let logins = page.agent_logins(cx).unwrap();
            assert_eq!(logins.len(), 1);
            assert_eq!(logins[0].username, "alice");
            // The same sign-in again offers nothing; a new password updates.
            page.password_report_for_test(report(1, submit("alice", "pw-1234")), cx);
            page.password_report_for_test(report(1, Signal::Gone), cx);
            assert_eq!(page.password_offer_for_test(), None);
            page.password_report_for_test(report(1, submit("alice", "pw-5678")), cx);
            page.password_report_for_test(report(1, Signal::Gone), cx);
            assert_eq!(
                page.password_offer_for_test().map(|o| o.2),
                Some(true),
                "an update"
            );
            page.save_offered(cx);
            assert_eq!(page.agent_logins(cx).unwrap()[0].password, "pw-5678");
        });
    }

    #[gpui::test]
    fn a_form_shown_again_after_navigating_is_not_offered(cx: &mut gpui::TestAppContext) {
        cx.update(vaults::open_for_test);
        let page = page("https://a.test/login", cx);
        page.update(cx, |page, cx| {
            page.password_report_for_test(report(1, Signal::Forms { password: true }), cx);
            page.password_report_for_test(report(1, submit("bob", "wrong-pw")), cx);
            // The next document shows the sign-in form again, with an error.
            page.password_report_for_test(report(2, Signal::Forms { password: true }), cx);
            assert_eq!(page.password_offer_for_test(), None);
        });
    }

    #[gpui::test]
    fn never_for_this_site_is_remembered(cx: &mut gpui::TestAppContext) {
        cx.update(vaults::open_for_test);
        let page = page("https://a.test/login", cx);
        page.update(cx, |page, cx| {
            page.password_report_for_test(report(1, submit("carol", "pw-9999")), cx);
            page.password_report_for_test(report(1, Signal::Gone), cx);
            page.never_offered(cx);
            assert_eq!(page.password_offer_for_test(), None);
            page.password_report_for_test(report(1, submit("carol", "pw-9999")), cx);
            page.password_report_for_test(report(1, Signal::Gone), cx);
            assert_eq!(page.password_offer_for_test(), None);
        });
    }

    #[gpui::test]
    fn the_password_manager_turned_off_offers_and_fills_nothing(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            vaults::open_for_test(cx);
            let mut prefs = crate::settings::Preferences::default();
            prefs.browser_offer_passwords = false;
            cx.set_global(prefs);
        });
        let page = page("https://a.test/login", cx);
        page.update(cx, |page, cx| {
            page.password_report_for_test(report(1, submit("dan", "pw-0000")), cx);
            page.password_report_for_test(report(1, Signal::Gone), cx);
            assert_eq!(page.password_offer_for_test(), None);
            assert!(page.agent_logins(cx).is_err());
        });
    }
}
