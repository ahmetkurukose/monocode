//! Settings → Passwords (docs/native/browser/context.md K55, K56, K60):
//! "Offer to save passwords", the saved logins of every browser profile
//! with a search, and the sites never saved for. Showing or copying a
//! password asks for Touch ID (or the account password) first, at most
//! once in five minutes; a copied one leaves the clipboard after two.
use crate::{
    browser::passwords::{self as vaults, VaultState},
    input::{self, Input},
    settings::{
        SettingsView,
        widgets::{icon_action, note, primary_button, red, secondary_button},
    },
    theme::*,
};
use gpui::{AnyElement, ClipboardItem, Context, Entity, Focusable, Window, div, prelude::*, px};
use std::{collections::HashMap, time::Duration};

/// A copied password leaves the clipboard after this long.
const CLIPBOARD_FOR: Duration = Duration::from_secs(120);

/// A login: its profile's vault and its id there.
type LoginKey = (String, i64);

struct Editing {
    key: LoginKey,
    username: Entity<Input>,
    password: Entity<Input>,
}

#[derive(Default)]
pub(in crate::settings) struct PasswordsState {
    search: Option<Entity<Input>>,
    /// Passwords shown after Touch ID, until the page closes.
    revealed: HashMap<LoginKey, String>,
    editing: Option<Editing>,
    copied: Option<LoginKey>,
    error: Option<String>,
}

impl SettingsView {
    fn password_search(&mut self, cx: &mut Context<Self>) -> Entity<Input> {
        if let Some(search) = &self.passwords.search {
            return search.clone();
        }
        let search = cx.new(|cx| {
            let mut field = Input::field("Search passwords", "Search sites and user names", content(0.35), cx);
            field.set_font_size(13.);
            field
        });
        self._subscriptions
            .push(cx.subscribe(&search, |_, _, _: &input::Changed, cx| cx.notify()));
        self.passwords.search = Some(search.clone());
        search
    }

    pub(in crate::settings) fn passwords_page(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let offer = self.prefs.browser_offer_passwords;
        let settings = vec![self.row(
            Some("browser-offer-passwords"),
            true,
            "Offer to save passwords",
            Some("After you sign in on a site, the browser offers to save the password, and suggests saved logins on sign-in forms. Agents can fill a saved login but never see it. Off, the browser neither saves nor fills passwords.".into()),
            vec![self.toggle(
                "browser-offer-passwords",
                "Offer to save passwords",
                offer,
                false,
                cx,
                |this, on, cx| this.update(cx, |p| p.browser_offer_passwords = on),
            )],
        )];
        let mut page = vec![self.group(None, true, "Browser", None, None, settings)];
        page.extend(self.saved_password_groups(cx));
        page
    }

    fn saved_password_groups(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let state = vaults::state(cx);
        let saved_any = vaults::saved_any(cx);
        let locked = |text: String, button: Option<&'static str>, danger: bool| {
            let mut row = div()
                .px_4()
                .py(px(14.))
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(12.))
                        .line_height(px(17.))
                        .text_color(if danger { red(1.) } else { content(0.55) })
                        .child(text),
                );
            if let Some(label) = button {
                row = row.child(
                    secondary_button("browser-passwords-unlock", label, false, false)
                        .child(label)
                        .on_click(cx.listener(|_, _, _, cx| {
                            let unlocking = vaults::unlock(cx);
                            cx.spawn(async move |this, cx| {
                                unlocking.await;
                                let _ = this.update(cx, |_, cx| cx.notify());
                            })
                            .detach();
                            cx.notify();
                        })),
                );
            }
            row.into_any_element()
        };
        let body = match &state {
            VaultState::Open => None,
            VaultState::Unlocking => Some(locked(
                "Unlocking your saved passwords… macOS may ask you to allow Monocode to use its Keychain item.".into(),
                None,
                false,
            )),
            VaultState::Unavailable(why) => Some(locked(why.clone(), Some("Try again"), true)),
            VaultState::Locked if saved_any => Some(locked(
                "Your saved passwords are locked until Monocode reads their key from the Keychain.".into(),
                Some("Unlock"),
                false,
            )),
            VaultState::Locked => Some(note(
                "No passwords saved yet. Sign in on a site in a browser tab and choose Save.",
            )),
        };
        if let Some(body) = body {
            return vec![self.group(
                Some("browser-saved-passwords"),
                false,
                "Saved passwords",
                None,
                None,
                vec![body],
            )];
        }
        let Some(open) = vaults::vaults(cx) else {
            return Vec::new();
        };
        let search = self.password_search(cx);
        let query = search.read(cx).content.to_lowercase();
        let (profiles, logins, never) = {
            let mut open = open.borrow_mut();
            let profiles = open.profiles();
            let mut logins = Vec::new();
            let mut never = Vec::new();
            for profile in &profiles {
                let Ok(vault) = open.vault(profile) else {
                    continue;
                };
                for login in vault.logins(None).unwrap_or_default() {
                    logins.push((profile.clone(), login));
                }
                for origin in vault.never_list().unwrap_or_default() {
                    never.push((profile.clone(), origin));
                }
            }
            (profiles, logins, never)
        };
        let several = profiles.len() > 1;
        let total = logins.len();
        let shown: Vec<_> = logins
            .into_iter()
            .filter(|(_, l)| {
                query.trim().is_empty()
                    || query.split_whitespace().all(|word| {
                        l.origin.to_lowercase().contains(word)
                            || l.username.to_lowercase().contains(word)
                    })
            })
            .collect();
        let mut rows = vec![
            div()
                .px_4()
                .py_2()
                .child(
                    div()
                        .h(px(32.))
                        .rounded_lg()
                        .border_1()
                        .border_color(content(0.15))
                        .bg(content(0.03))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon("search", 13.))
                        .child(div().flex_1().min_w_0().child(search.clone())),
                )
                .into_any_element(),
        ];
        if let Some(error) = &self.passwords.error {
            rows.push(
                div()
                    .px_4()
                    .text_size(px(12.))
                    .text_color(red(1.))
                    .child(error.clone())
                    .into_any_element(),
            );
        }
        if shown.is_empty() {
            rows.push(note(if total == 0 {
                "No passwords saved yet. Sign in on a site in a browser tab and choose Save."
            } else {
                "No saved password matches."
            }));
        }
        for (ix, (profile, login)) in shown.iter().enumerate() {
            rows.push(self.login_row(ix, profile, login, several, cx));
        }
        let description = format!(
            "{total} saved login{}. Showing or copying a password asks for Touch ID or your Mac's password first.",
            if total == 1 { "" } else { "s" }
        );
        let mut groups = vec![self.group(
            Some("browser-saved-passwords"),
            false,
            "Saved passwords",
            Some(description.into()),
            None,
            rows,
        )];
        let never_rows: Vec<AnyElement> = if never.is_empty() {
            vec![note("When you choose \"Never for this site\", the site shows here.")]
        } else {
            never
                .iter()
                .enumerate()
                .map(|(ix, (profile, origin))| {
                    let (profile, origin) = (profile.clone(), origin.clone());
                    let id = gpui::ElementId::NamedInteger("browser-never-remove".into(), ix as u64);
                    self.row(
                        None,
                        ix == 0,
                        monocode_core::browser::sites::host_of(&origin),
                        several.then(|| format!("{profile} profile").into()),
                        vec![crate::modal::ghost_button(id, "Remove")
                            .cursor_pointer()
                            .on_click(cx.listener(move |_, _, _, cx| {
                                let _ = vaults::with_vault(&profile, cx, |v| v.allow_saving(&origin));
                                vaults::changed(cx);
                            }))
                            .into_any_element()],
                    )
                })
                .collect()
        };
        groups.push(self.group(
            Some("browser-never-saved"),
            false,
            "Never saved",
            Some("Sites the browser does not offer to save passwords for.".into()),
            None,
            never_rows,
        ));
        groups
    }

    fn login_row(
        &mut self,
        ix: usize,
        profile: &str,
        login: &monocode_core::browser::passwords::Login,
        several: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key: LoginKey = (profile.to_owned(), login.id);
        if self.passwords.editing.as_ref().is_some_and(|e| e.key == key) {
            return self.edit_row(ix, login, cx);
        }
        let revealed = self.passwords.revealed.get(&key).cloned();
        let site = monocode_core::browser::sites::host_of(&login.origin);
        let user = if login.username.is_empty() {
            "(no user name)".to_owned()
        } else {
            login.username.clone()
        };
        let mut description = user;
        if several {
            description.push_str(&format!(" · {profile} profile"));
        }
        let copied = self.passwords.copied.as_ref() == Some(&key);
        let id = |name: &'static str| gpui::ElementId::NamedInteger(name.into(), ix as u64);
        let (k1, k2, k3, k4) = (key.clone(), key.clone(), key.clone(), key.clone());
        let label = div()
            .flex()
            .flex_col()
            .child(site)
            .when_some(revealed.clone(), |d, password| {
                d.child(
                    div()
                        .mt_1()
                        .font_family("Menlo")
                        .text_size(px(12.))
                        .text_color(content(0.85))
                        .child(password),
                )
            });
        self.row(
            None,
            ix == 0,
            label,
            Some(description.into()),
            vec![
                icon_action(
                    id("browser-password-reveal"),
                    if revealed.is_some() { "Hide password" } else { "Show password" },
                    "eye",
                    26.,
                    14.,
                    if revealed.is_some() { 0.9 } else { 0.55 },
                    false,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.passwords.revealed.remove(&k1).is_some() {
                        cx.notify();
                        return;
                    }
                    let key = k1.clone();
                    this.with_password(key.clone(), "show a saved password", cx, move |this, password, cx| {
                        this.passwords.revealed.insert(key, password);
                        cx.notify();
                    });
                }))
                .into_any_element(),
                icon_action(
                    id("browser-password-copy"),
                    if copied { "Copied" } else { "Copy password" },
                    if copied { "check" } else { "response-copy" },
                    26.,
                    14.,
                    0.55,
                    false,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let key = k2.clone();
                    this.with_password(key.clone(), "copy a saved password", cx, move |this, password, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(password.clone()));
                        this.passwords.copied = Some(key);
                        cx.spawn(async move |this, cx| {
                            cx.background_executor().timer(CLIPBOARD_FOR).await;
                            let _ = this.update(cx, |this, cx| {
                                let still = cx
                                    .read_from_clipboard()
                                    .and_then(|c| c.text())
                                    .is_some_and(|t| t == password);
                                if still {
                                    cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
                                }
                                this.passwords.copied = None;
                                cx.notify();
                            });
                        })
                        .detach();
                        cx.notify();
                    });
                }))
                .into_any_element(),
                icon_action(id("browser-password-edit"), "Edit", "pencil-edit-02", 26., 14., 0.55, false)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let key = k3.clone();
                        let window = window.window_handle();
                        this.with_password(key.clone(), "edit a saved password", cx, move |this, password, cx| {
                            this.start_editing(key, password, window, cx);
                        });
                    }))
                    .into_any_element(),
                icon_action(id("browser-password-delete"), "Delete", "delete-02", 26., 14., 0.55, false)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (profile, id) = k4.clone();
                        if let Some(Err(error)) = vaults::with_vault(&profile, cx, |v| v.delete(id)) {
                            this.passwords.error = Some(error);
                        }
                        this.passwords.revealed.remove(&k4);
                        vaults::changed(cx);
                    }))
                    .into_any_element(),
            ],
        )
    }

    /// Runs `then` with login `key`'s password once the user proved it is
    /// them.
    fn with_password(
        &mut self,
        key: LoginKey,
        reason: &str,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, String, &mut Context<Self>) + 'static,
    ) {
        let check = vaults::authenticate(&format!("Monocode wants to {reason}"), cx);
        cx.spawn(async move |this, cx| {
            if !check.await {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                match vaults::with_vault(&key.0, cx, |v| v.password(key.1)) {
                    Some(Ok(password)) => {
                        this.passwords.error = None;
                        then(this, password, cx);
                    }
                    Some(Err(error)) => {
                        this.passwords.error = Some(error);
                        cx.notify();
                    }
                    None => {}
                }
            });
        })
        .detach();
    }

    fn start_editing(
        &mut self,
        key: LoginKey,
        password: String,
        window: gpui::AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let username = vaults::with_vault(&key.0, cx, |v| v.get(key.1))
            .and_then(Result::ok)
            .flatten()
            .map(|l| l.username)
            .unwrap_or_default();
        let field = |name: &'static str, text: String, secret: bool, cx: &mut Context<Self>| {
            cx.new(|cx| {
                let mut input = Input::field(name, name, content(0.35), cx);
                input.set_font_size(13.);
                input.set_secret(secret);
                input.set_text(text, cx);
                input
            })
        };
        let username = field("User name", username, false, cx);
        let password = field("Password", password, false, cx);
        let focus = username.focus_handle(cx);
        let _ = window.update(cx, |_, window, cx| window.focus(&focus, cx));
        self.passwords.editing = Some(Editing {
            key,
            username,
            password,
        });
        cx.notify();
    }

    fn edit_row(
        &mut self,
        ix: usize,
        login: &monocode_core::browser::passwords::Login,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(editing) = &self.passwords.editing else {
            return div().into_any_element();
        };
        let boxed = |input: Entity<Input>| {
            div()
                .h(px(32.))
                .flex_1()
                .min_w_0()
                .rounded_lg()
                .border_1()
                .border_color(content(0.15))
                .bg(content(0.03))
                .px_3()
                .flex()
                .items_center()
                .child(input)
        };
        let (username, password) = (editing.username.clone(), editing.password.clone());
        div()
            .id(("browser-password-editing", ix))
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .when(ix > 0, |d| d.border_t_1().border_color(content(0.08)))
            .child(
                div()
                    .text_size(px(13.))
                    .child(monocode_core::browser::sites::host_of(&login.origin)),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(boxed(username))
                    .child(boxed(password)),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        secondary_button("browser-password-edit-cancel", "Cancel", false, false)
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.passwords.editing = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        primary_button("browser-password-edit-save", "Save", false)
                            .child("Save")
                            .on_click(cx.listener(|this, _, _, cx| this.save_edit(cx))),
                    ),
            )
            .into_any_element()
    }

    fn save_edit(&mut self, cx: &mut Context<Self>) {
        let Some(editing) = self.passwords.editing.take() else {
            return;
        };
        let username = editing.username.read(cx).content.trim().to_owned();
        let password = editing.password.read(cx).content.to_string();
        let now = monocode_core::time::now_ms() as i64;
        let (profile, id) = editing.key.clone();
        match vaults::with_vault(&profile, cx, |v| v.edit(id, &username, &password, now)) {
            Some(Err(error)) => {
                self.passwords.error = Some(error);
                self.passwords.editing = Some(editing);
            }
            _ => {
                self.passwords.error = None;
                self.passwords.revealed.remove(&editing.key);
            }
        }
        vaults::changed(cx);
        cx.notify();
    }
}
