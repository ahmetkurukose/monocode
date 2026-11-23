//! The three application destinations share one navigation and focus boundary.
use super::tab_router::OpenIn;
use super::*;
use crate::{automations, inbox, notes};

pub(super) use crate::workspace::AppView;

impl Shell {
    /// The app view in the focused pane.
    pub(super) fn active_app_view(&self) -> Option<AppView> {
        match self.focused_view()? {
            TitleTab::App(view) | TitleTab::ProjectApp { view, .. } => Some(*view),
            _ => None,
        }
    }

    /// Whether an app view's pane has focus.
    pub(super) fn app_view_open(&self) -> bool {
        self.active_app_view().is_some()
    }

    pub(super) fn app_view_label(&self) -> Option<&'static str> {
        self.active_app_view().map(AppView::label)
    }

    /// Reads how many notes the store holds for the sidebar's Notes row.
    pub(super) fn load_note_count(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let work = cx
            .background_executor()
            .spawn(async move { locked(&store)?.list_notes().map(|notes| notes.len()) });
        cx.spawn(async move |this, cx| {
            let Ok(count) = work.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.note_count != Some(count) {
                    this.note_count = Some(count);
                    this.notify_sidebar(cx);
                }
            });
        })
        .detach();
    }

    /// Notes saves what it holds whenever its pane loses focus or closes.
    pub(super) fn leave_app_views(&mut self, cx: &mut Context<Self>) {
        if self.active_app_view() == Some(AppView::Notes)
            && let Some(notes) = &self.notes
        {
            notes.update(cx, |notes, cx| notes.flush(cx));
        }
        self.invalidate_chrome(cx);
    }

    /// Close the focused app view's tab; its group's next tab takes over.
    pub(super) fn close_app_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.active_app_view() {
            self.close_view_tab(&TitleTab::App(view), window, cx);
        }
    }

    fn prepare_app_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_preview(cx);
        self.close_menu(cx);
        if self.settings.is_some() {
            self.close_settings(window, cx);
        }
        self.navigation_revision += 1;
        self.pending_composer_focus = None;
    }

    /// Open `view` as a tab of the current scope (one per scope).
    fn show_app_view(&mut self, view: AppView, window: &mut Window, cx: &mut Context<Self>) {
        self.open_view_tab(TitleTab::App(view), OpenIn::default(), window, cx);
    }

    /// Open any app view, as the sidebar, rail and menus do.
    pub(super) fn open_app(&mut self, view: AppView, window: &mut Window, cx: &mut Context<Self>) {
        match view {
            AppView::Inbox => self.open_inbox(window, cx),
            AppView::Notes => self.open_notes(window, cx),
            AppView::Automations => self.open_automations(window, cx),
            AppView::Database => self.open_database(window, cx),
            AppView::Api => self.open_api(window, cx),
        }
    }

    /// Make `view`'s entity if need be, without refreshing it. False when
    /// it cannot be made yet (Automations before its service is up).
    pub(super) fn ensure_app_entity(
        &mut self,
        view: AppView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match view {
            AppView::Notes => {
                if self.notes.is_none() {
                    self.make_notes(window, cx);
                    self.ready_notes(cx);
                }
            }
            AppView::Inbox => self.ensure_background_inbox(window, cx),
            AppView::Automations => {
                if self.automations.is_none() {
                    let Some(service) = self.automation_state.service.clone() else {
                        self.initialize_automations(cx);
                        return false;
                    };
                    self.make_automations(service, window, cx);
                    self.ready_automations(cx);
                }
            }
            AppView::Database => {
                self.ensure_database(window, cx);
            }
            AppView::Api => {
                self.ensure_api(cx);
            }
        }
        true
    }

    /// Make `view` ready for the current scope: made, given the project
    /// and refreshed.
    pub(super) fn ready_app_view(
        &mut self,
        view: AppView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.ensure_app_entity(view, window, cx) {
            return;
        }
        match view {
            AppView::Notes => self.ready_notes(cx),
            AppView::Inbox => {
                if let Some(inbox) = &self.inbox {
                    inbox.update(cx, |inbox, cx| inbox.refresh(cx));
                }
            }
            AppView::Automations => self.ready_automations(cx),
            AppView::Database => self.ready_database(window, cx),
            AppView::Api => self.ready_api(cx),
        }
    }

    /// `view`'s entity as an element, once it is made.
    pub(super) fn app_view_element(&self, view: AppView) -> Option<AnyElement> {
        match view {
            AppView::Notes => self.notes.clone().map(IntoElement::into_any_element),
            AppView::Inbox => self.inbox.clone().map(IntoElement::into_any_element),
            AppView::Automations => self.automations.clone().map(IntoElement::into_any_element),
            AppView::Database => self.database.clone().map(IntoElement::into_any_element),
            AppView::Api => self.api.clone().map(IntoElement::into_any_element),
        }
    }

    /// An app view's entity, when it has been made.
    pub(super) fn app_view_handle(&self, view: AppView) -> Option<gpui::AnyView> {
        match view {
            AppView::Notes => self.notes.clone().map(Into::into),
            AppView::Inbox => self.inbox.clone().map(Into::into),
            AppView::Automations => self.automations.clone().map(Into::into),
            AppView::Database => self.database.clone().map(Into::into),
            AppView::Api => self.api.clone().map(Into::into),
        }
    }

    /// The focused app view (or Search page) takes the keyboard. False when
    /// no view pane has focus.
    pub(super) fn focus_app_view(&self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(pane) = self.workspace.active
            && self.workspace.view_panes.get(&pane) == Some(&TitleTab::Search)
        {
            if let Some(open) = self.search_panes.get(&pane) {
                window.focus(&open.view.focus_handle(cx), cx);
            }
            return true;
        }
        let focus = match self.active_app_view() {
            Some(AppView::Notes) => self.notes.as_ref().map(|view| view.focus_handle(cx)),
            Some(AppView::Inbox) => self.inbox.as_ref().map(|view| view.focus_handle(cx)),
            Some(AppView::Automations) => {
                self.automations.as_ref().map(|view| view.focus_handle(cx))
            }
            Some(AppView::Database) => self.database.as_ref().map(|view| view.focus_handle(cx)),
            Some(AppView::Api) => self.api.as_ref().map(|view| view.focus_handle(cx)),
            None => return false,
        };
        if let Some(focus) = focus {
            window.focus(&focus, cx);
        }
        true
    }

    fn make_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = cx.new(|cx| notes::Notes::new(self.store.clone(), cx));
        self.app_view_events.push(cx.subscribe_in(
            &view,
            window,
            |this, _, event: &notes::Event, window, cx| match event {
                notes::Event::Close => {
                    this.close_view_tab(&TitleTab::App(AppView::Notes), window, cx)
                }
                notes::Event::OpenSource(id) => this.open(*id, window, cx),
                notes::Event::AddToChat(note) => this.note_to_chat(note.clone(), window, cx),
                notes::Event::OpenIntegrations => {
                    this.open_settings(crate::settings::Section::Integrations, window, cx)
                }
            },
        ));
        self.app_view_events
            .push(cx.observe(&view, |this, notes, cx| {
                if let Some(count) = notes.read(cx).count()
                    && this.note_count != Some(count)
                {
                    this.note_count = Some(count);
                    this.notify_sidebar(cx);
                }
            }));
        self.notes = Some(view);
    }

    fn ready_notes(&mut self, cx: &mut Context<Self>) {
        if let Some(notes) = &self.notes {
            let cwd = self.workspace.project.clone();
            let projects = self.workspace.projects.clone();
            let scope = self.project_app_scope(AppView::Notes).then(|| cwd.clone());
            notes.update(cx, |notes, cx| {
                notes.set_appearances(self.workspace.project_appearance.clone(), cx);
                notes.set_context(cwd, projects, cx);
                notes.set_scope(scope, cx);
                notes.refresh(cx);
            });
        }
    }

    pub(super) fn open_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_app_view(window, cx);
        self.ready_app_view(AppView::Notes, window, cx);
        self.show_app_view(AppView::Notes, window, cx);
    }

    pub(super) fn open_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_app_view(window, cx);
        self.ensure_background_inbox(window, cx);
        if let Some(inbox) = &self.inbox {
            inbox.update(cx, |inbox, cx| inbox.refresh(cx));
        }
        self.show_app_view(AppView::Inbox, window, cx);
    }

    /// Open the Inbox with `pr` selected.
    pub(super) fn reveal_pull_request(
        &mut self,
        pr: inbox::RevealPr,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_inbox(window, cx);
        if let Some(inbox) = &self.inbox {
            inbox.update(cx, |inbox, cx| inbox.reveal_pull_request(pr, cx));
        }
    }

    pub(super) fn ensure_background_inbox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inbox.is_none() {
            let projects = self.workspace.projects.iter().map(Into::into).collect();
            let cwd = self.workspace.project.clone().into();
            let view = cx.new(|cx| inbox::Inbox::new(self.store.clone(), projects, cwd, cx));
            self.app_view_events.push(cx.subscribe_in(
                &view,
                window,
                |this, _, event: &inbox::Event, window, cx| match event {
                    inbox::Event::Close => {
                        this.close_view_tab(&TitleTab::App(AppView::Inbox), window, cx)
                    }
                    inbox::Event::OpenSession(id) => this.open(*id, window, cx),
                    inbox::Event::StartSession { item, prompt } => {
                        this.start_inbox_session(item.as_ref().clone(), prompt.clone(), cx);
                    }
                    inbox::Event::CheckoutPr(item) => {
                        this.checkout_pr(item.as_ref().clone(), cx);
                    }
                    inbox::Event::ItemsChanged(items) => {
                        this.inbox_automation_events(items.clone(), cx)
                    }
                },
            ));
            self.app_view_events
                .push(cx.observe(&view, |this, inbox, cx| {
                    if let Some(automations) = &this.automations {
                        let connections = inbox
                            .read(cx)
                            .connection_statuses()
                            .into_iter()
                            .map(|(provider, (connected, _))| (provider, connected))
                            .collect();
                        automations
                            .update(cx, |view, cx| view.set_trigger_connections(connections, cx));
                    }
                    this.invalidate_chrome(cx);
                    this.sync_settings(cx);
                    cx.notify();
                }));
            self.inbox = Some(view);
        }
        if let Some(inbox) = &self.inbox {
            let projects = self.workspace.projects.iter().map(Into::into).collect();
            let cwd = self.workspace.project.clone().into();
            inbox.update(cx, |inbox, cx| {
                inbox.set_appearances(self.workspace.project_appearance.clone(), cx);
                inbox.set_context(projects, cwd, cx);
            });
        }
    }

    pub(super) fn open_automations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.automation_state.service.is_none() {
            self.automation_state.open_requested = Some(self.navigation_revision);
            self.initialize_automations(cx);
            return;
        }
        self.prepare_app_view(window, cx);
        self.ready_app_view(AppView::Automations, window, cx);
        self.show_app_view(AppView::Automations, window, cx);
    }

    fn make_automations(
        &mut self,
        service: super::automation_runs::ServiceHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = self.workspace.project.clone();
        let projects = self.workspace.projects.clone();
        let view = cx.new(|cx| automations::Automations::new(service, cwd, projects, cx));
        view.update(cx, |view, cx| view.set_database(self.store.clone(), cx));
        self.app_view_events.push(cx.subscribe_in(
            &view,
            window,
            |this, _, event: &automations::Event, window, cx| match event {
                automations::Event::Close => {
                    this.close_view_tab(&TitleTab::App(AppView::Automations), window, cx)
                }
                automations::Event::Launch(run) => {
                    this.launch_automation(run.as_ref().clone(), true, cx)
                }
                automations::Event::OpenSession(id) => {
                    let native = locked(&this.store)
                        .ok()
                        .and_then(|store| store.resolve_existing_session_id(id));
                    if let Some(id) = native {
                        this.open(id, window, cx);
                    }
                }
            },
        ));
        self.automations = Some(view);
    }

    fn ready_automations(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = &self.automations {
            let cwd = self.workspace.project.clone();
            let projects = self.workspace.projects.clone();
            view.update(cx, |view, cx| {
                view.set_appearances(self.workspace.project_appearance.clone(), cx);
                let connections = self
                    .inbox
                    .as_ref()
                    .map(|inbox| inbox.read(cx).connection_statuses())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(provider, (connected, _))| (provider, connected))
                    .collect();
                view.set_trigger_connections(connections, cx);
                view.set_context(cwd, projects, cx);
                view.refresh(cx);
            });
        }
    }

    /// The Database view, made if need be, without showing it.
    pub(super) fn ensure_database(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<crate::database::DatabaseView> {
        if let Some(view) = &self.database {
            return view.clone();
        }
        let dir = self.database_dir.clone();
        let view = cx.new(|cx| crate::database::DatabaseView::new(dir, cx));
        self.app_view_events.push(cx.subscribe_in(
            &view,
            window,
            |this, _, event: &crate::database::Event, window, cx| match event {
                crate::database::Event::Close => {
                    this.close_view_tab(&TitleTab::App(AppView::Database), window, cx)
                }
            },
        ));
        self.database = Some(view.clone());
        view
    }

    pub(super) fn open_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_app_view(window, cx);
        self.ready_app_view(AppView::Database, window, cx);
        self.show_app_view(AppView::Database, window, cx);
    }

    fn ready_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = self.ensure_database(window, cx);
        let scope =
            (self.project_app_scope(AppView::Database)).then(|| self.workspace.project.clone());
        let projects = self.workspace.projects.clone();
        view.update(cx, |view, cx| view.set_scope(scope, projects, cx));
    }

    /// The API view, made if need be, without showing it.
    pub(super) fn ensure_api(&mut self, cx: &mut Context<Self>) -> Entity<crate::api::ApiView> {
        if let Some(view) = &self.api {
            return view.clone();
        }
        let dir = self.api_state_dir.clone();
        let view = cx.new(|cx| crate::api::ApiView::new(dir, cx));
        self.app_view_events.push(
            cx.subscribe(
                &view,
                |this, _, event: &crate::api::Event, cx| match event {
                    crate::api::Event::AskAgent {
                        project,
                        title,
                        prompt,
                    } => this.start_api_chat(project.clone(), title.clone(), prompt.clone(), cx),
                },
            ),
        );
        self.api = Some(view.clone());
        view
    }

    /// Opens the API view and adds the cURL command to the project's collection.
    pub(super) fn send_curl_to_api(
        &mut self,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_api(window, cx);
        if let Some(view) = self.api.clone() {
            view.update(cx, |view, cx| {
                view.import_curl_request(&command, window, cx)
            });
        }
    }

    /// A new chat in `project` with `prompt` in its composer, not yet sent.
    fn start_api_chat(
        &mut self,
        project: String,
        title: String,
        prompt: String,
        cx: &mut Context<Self>,
    ) {
        let choice = self.new_chat_choice(&project);
        let kind = choice.kind;
        let settings = monocode_core::agent::Settings {
            model: choice.model,
            effort: crate::conversation::preferred_effort(kind.id(), cx),
            access: self
                .workspace
                .preferences
                .default_access
                .as_deref()
                .map(monocode_core::agent::AccessMode::from_id)
                .unwrap_or_default(),
            account: crate::accounts::get(cx)
                .map(|accounts| accounts.read(cx).selected(kind, &project))
                .unwrap_or_default(),
            ..Default::default()
        };
        let store = self.store.clone();
        self.navigation_revision += 1;
        let revision = self.navigation_revision;
        let work = cx.background_executor().spawn(async move {
            if !std::path::Path::new(&project).is_dir() {
                return Err("Choose a project folder first".to_owned());
            }
            let store = locked(&store)?;
            let mut summary = store.create(kind.id(), &project)?;
            store.title(summary.id, &title)?;
            summary.title = title;
            store.save_agent_settings(summary.id, &settings)?;
            Ok::<_, String>((summary, settings, prompt))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((summary, settings, prompt)) => {
                    let id = summary.id;
                    this.draft_sessions.insert(id);
                    this.add(summary, cx);
                    this.sessions[&id].update(cx, |view, cx| {
                        view.install_transfer(settings, Some(prompt), cx)
                    });
                    if this.navigation_revision == revision {
                        this.activate_session(id, window, cx);
                    }
                    this.persist(cx);
                }
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn open_api(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prepare_app_view(window, cx);
        self.ready_app_view(AppView::Api, window, cx);
        self.show_app_view(AppView::Api, window, cx);
    }

    fn ready_api(&mut self, cx: &mut Context<Self>) {
        let view = self.ensure_api(cx);
        let scope = (self.project_app_scope(AppView::Api)).then(|| self.workspace.project.clone());
        let projects = self.workspace.projects.clone();
        let current = self.workspace.project.clone();
        view.update(cx, |view, cx| view.set_scope(scope, projects, current, cx));
    }

    fn note_to_chat(
        &mut self,
        note: monocode_core::notes::Note,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = note
            .source_cwd
            .clone()
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| self.workspace.project.clone());
        let choice = self.new_chat_choice(&cwd);
        let kind = choice.kind;
        let store = self.store.clone();
        let settings = monocode_core::agent::Settings {
            model: choice.model,
            effort: crate::conversation::preferred_effort(kind.id(), cx),
            access: self
                .workspace
                .preferences
                .default_access
                .as_deref()
                .map(monocode_core::agent::AccessMode::from_id)
                .unwrap_or_default(),
            account: crate::accounts::get(cx)
                .map(|accounts| accounts.read(cx).selected(kind, &cwd))
                .unwrap_or_default(),
            ..Default::default()
        };
        self.leave_app_views(cx);
        self.navigation_revision += 1;
        let revision = self.navigation_revision;
        let work = cx.background_executor().spawn(async move {
            let store = locked(&store)?;
            let mut summary = store.create(kind.id(), &cwd)?;
            if !note.title.trim().is_empty() {
                store.title(summary.id, &note.title)?;
                summary.title = note.title.clone();
            }
            store.save_agent_settings(summary.id, &settings)?;
            Ok::<_, String>((summary, settings, note))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((summary, settings, note)) => {
                    let id = summary.id;
                    this.add(summary, cx);
                    this.sessions[&id].update(cx, |view, cx| {
                        view.seed_new_settings(settings, cx);
                        view.input.update(cx, |input, cx| {
                            input.set_text(format!("@note/{} ", note.slug), cx)
                        });
                    });
                    if this.navigation_revision == revision {
                        this.activate_session(id, window, cx);
                    }
                    this.persist(cx);
                }
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
        self.update_chat_visibility(cx);
        self.focus_active(window, cx);
    }

    fn start_inbox_session(
        &mut self,
        item: monocode_core::inbox::Item,
        prompt: String,
        cx: &mut Context<Self>,
    ) {
        let cwd = if item.project_path.is_empty() {
            self.workspace.project.clone()
        } else {
            item.project_path.clone()
        };
        let choice = self.new_chat_choice(&cwd);
        let kind = choice.kind;
        let settings = monocode_core::agent::Settings {
            model: choice.model,
            effort: crate::conversation::preferred_effort(kind.id(), cx),
            access: self
                .workspace
                .preferences
                .default_access
                .as_deref()
                .map(monocode_core::agent::AccessMode::from_id)
                .unwrap_or_default(),
            account: crate::accounts::get(cx)
                .map(|accounts| accounts.read(cx).selected(kind, &cwd))
                .unwrap_or_default(),
            ..Default::default()
        };
        let store = self.store.clone();
        self.navigation_revision += 1;
        let revision = self.navigation_revision;
        let work = cx.background_executor().spawn(async move {
            if !std::path::Path::new(&cwd).is_dir() {
                return Err("Choose a project folder before starting work on this item".into());
            }
            let store = locked(&store)?;
            let mut summary = store.create(kind.id(), &cwd)?;
            summary.title = format!("{} · {}", item.reference(), item.title);
            summary.work_item = Some(item.work_item());
            store.title(summary.id, &summary.title)?;
            store.set_work_item(summary.id, summary.work_item.as_ref())?;
            store.save_agent_settings(summary.id, &settings)?;
            Ok::<_, String>((summary, settings, prompt))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok((summary, settings, prompt)) => {
                    let id = summary.id;
                    this.add(summary, cx);
                    this.sessions[&id]
                        .update(cx, |view, cx| view.quick_submit(prompt, settings, cx));
                    if this.navigation_revision == revision {
                        this.activate_session(id, window, cx);
                    }
                    this.persist(cx);
                }
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            });
        })
        .detach();
    }
}
