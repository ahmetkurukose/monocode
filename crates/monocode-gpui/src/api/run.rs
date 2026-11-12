//! Sending a request and what comes of it: scripts, the response, and the
//! variables the scripts set.

use super::*;
use api::VarChange;

/// What a script set or removed, applied to a plain variable list.
fn apply_to(vars: &mut Vec<KeyValue>, changes: &[VarChange]) {
    for change in changes {
        match &change.value {
            Some(value) => match vars.iter_mut().find(|v| v.key == change.key) {
                Some(row) => row.value = value.clone(),
                None => vars.push(KeyValue::new(change.key.clone(), value.clone())),
            },
            None => vars.retain(|v| v.key != change.key),
        }
    }
}

impl ApiView {
    pub(super) fn variables_scope(&self, file: &Path, path: &[usize]) -> Scope {
        let mut scope = Scope::new();
        scope.push(self.globals.clone());
        if let Some(loaded) = self.collection(file) {
            scope.push(loaded.collection.variables(path));
            if let Some(env) = self.environment_for(&loaded.project) {
                scope.push(env.environment.variables());
            }
        }
        scope
    }

    fn effective_auth(&self, file: &Path, path: &[usize], own: &Auth) -> Auth {
        match own {
            Auth::Inherit => self
                .collection(file)
                .map(|c| c.collection.inherited_auth(path))
                .unwrap_or(Auth::None),
            own => own.clone(),
        }
    }

    /// The request as it would go out, before any script has run.
    pub(super) fn prepare_tab(&self, tab: &RequestTab) -> api::Prepared {
        let scope = self.variables_scope(&tab.file, &tab.path);
        let auth = self.effective_auth(&tab.file, &tab.path, &tab.draft.auth);
        api::prepare(&tab.draft, &auth, &scope)
    }

    /// The scripts of one kind that run for a tab, outermost first. The
    /// request's own is the draft's, not the saved one.
    pub(super) fn scripts_for(&self, tab: &RequestTab, listen: &str) -> Vec<String> {
        let mut scripts: Vec<String> = self
            .collection(&tab.file)
            .map(|c| {
                c.collection
                    .scripts_above(&tab.path, listen)
                    .into_iter()
                    .map(|(_, script)| script)
                    .collect()
            })
            .unwrap_or_default();
        let own = if listen == "prerequest" {
            &tab.draft.pre_request
        } else {
            &tab.draft.tests
        };
        scripts.push(own.clone());
        scripts
    }

    /// Whom the scripts above a request belong to, for the Scripts tab.
    pub(super) fn script_owners(&self, tab: &RequestTab, listen: &str) -> Vec<String> {
        self.collection(&tab.file)
            .map(|c| {
                c.collection
                    .scripts_above(&tab.path, listen)
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn exchange_input(&self, tab: &RequestTab) -> api::ExchangeInput {
        let collection = self.collection(&tab.file);
        let environment = collection.and_then(|c| self.environment_for(&c.project));
        api::ExchangeInput {
            request: tab.draft.clone(),
            inherited_auth: collection
                .map(|c| c.collection.inherited_auth(&tab.path))
                .unwrap_or(Auth::None),
            collection_vars: collection
                .map(|c| c.collection.variables(&tab.path))
                .unwrap_or_default(),
            environment_name: environment
                .map(|e| e.environment.name())
                .unwrap_or_default(),
            environment_vars: environment
                .map(|e| e.environment.variables())
                .unwrap_or_default(),
            globals: self.globals.clone(),
            pre_scripts: self.scripts_for(tab, "prerequest"),
            test_scripts: self.scripts_for(tab, "test"),
            options: SendOptions::default(),
        }
    }

    pub(super) fn send(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let input = self.exchange_input(tab);
        let Some(tab) = self.tab_mut(id) else {
            return;
        };
        tab.generation += 1;
        let generation = tab.generation;
        tab.exchange = Exchange::Sending;
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(api::exchange::run(input));
        });
        cx.spawn(async move |this, cx| {
            let output = rx.recv().await;
            let _ = this.update(cx, |this, cx| this.finish(id, generation, output.ok(), cx));
        })
        .detach();
        cx.notify();
    }

    /// Stops waiting for the answer. The request itself runs to its timeout.
    pub(super) fn cancel(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut(id)
            && matches!(tab.exchange, Exchange::Sending)
        {
            tab.generation += 1;
            tab.exchange = Exchange::Idle;
            cx.notify();
        }
    }

    pub(super) fn finish(
        &mut self,
        id: u64,
        generation: u64,
        output: Option<api::ExchangeOutput>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tab_mut(id) else {
            return;
        };
        if tab.generation != generation {
            return;
        }
        let file = tab.file.clone();
        let Some(output) = output else {
            tab.exchange = Exchange::Failed("The request was interrupted.".into());
            cx.notify();
            return;
        };
        tab.tests = output.tests.clone();
        tab.logs = output.logs.clone();
        tab.exchange = match &output.outcome {
            Err(error) => Exchange::Failed(error.to_string()),
            Ok(response) => Exchange::Done(Box::new(summarize(response))),
        };
        tab.response_tab = ResponseTab::Pretty;
        self.apply_changes(&file, &output, cx);
        self.show_response(id, cx);
        cx.notify();
    }

    /// Saves the variables the scripts set: into the project's environment
    /// when one is chosen (as Postman does), else for this session only.
    fn apply_changes(&mut self, file: &Path, output: &api::ExchangeOutput, cx: &mut Context<Self>) {
        let Some(project) = self.collection(file).map(|c| c.project.clone()) else {
            return;
        };
        if !output.environment.is_empty() {
            let chosen = self.environment.get(&project).cloned();
            let target = chosen.and_then(|f| self.environments.iter().position(|e| e.file == f));
            match target {
                Some(index) => {
                    let env = &mut self.environments[index];
                    env.environment.apply(&output.environment);
                    let (file, saved) = (env.file.clone(), env.environment.save());
                    self.touch(&file);
                    if let Err(error) = saved {
                        self.notify_user(error.to_string(), cx);
                    }
                }
                None => apply_to(&mut self.globals, &output.environment),
            }
        }
        if !output.collection.is_empty() {
            if let Some(loaded) = self.collection_mut(file) {
                loaded.collection.apply_variables(&output.collection);
            }
            self.save_collection(file, cx);
        }
        apply_to(&mut self.globals, &output.globals);
    }

    /// What an agent is told about a request: as typed, with credentials
    /// hidden, and what came back if it was sent.
    pub(super) fn agent_prompt(&self, tab: &RequestTab) -> String {
        use std::fmt::Write as _;
        let hidden = |name: &str| {
            let n = name.to_ascii_lowercase();
            n == "authorization"
                || n == "cookie"
                || n == "set-cookie"
                || ["token", "secret", "api-key", "apikey", "password"]
                    .iter()
                    .any(|word| n.contains(word))
        };
        let clip = |text: &str, limit: usize| -> String {
            if text.chars().count() > limit {
                format!("{}\n… (cut)", text.chars().take(limit).collect::<String>())
            } else {
                text.to_owned()
            }
        };
        let collection = self
            .collection(&tab.file)
            .map(|c| c.collection.name())
            .unwrap_or_default();
        let draft = &tab.draft;
        let mut out = format!(
            "I'm working on this API request, “{}”, from the {collection} collection.\n\n",
            draft.name
        );
        let _ = writeln!(
            out,
            "Request:\n```http\n{} {}",
            draft.method,
            draft.url_text()
        );
        for header in draft
            .headers
            .iter()
            .filter(|h| !h.disabled && !h.key.is_empty())
        {
            let value = if hidden(&header.key) {
                "[hidden]"
            } else {
                &header.value
            };
            let _ = writeln!(out, "{}: {value}", header.key);
        }
        match &draft.auth {
            Auth::Inherit | Auth::None => {}
            other => {
                let _ = writeln!(
                    out,
                    "(auth: {}, credentials hidden)",
                    other.label().to_lowercase()
                );
            }
        }
        match &draft.body {
            Body::Raw { text, .. } if !text.trim().is_empty() => {
                let _ = writeln!(out, "\n{}", clip(text, 4000));
            }
            Body::UrlEncoded(fields) if !fields.is_empty() => {
                let _ = writeln!(
                    out,
                    "\n(form fields: {})",
                    fields
                        .iter()
                        .map(|f| f.key.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Body::FormData(parts) if !parts.is_empty() => {
                let _ = writeln!(
                    out,
                    "\n(multipart fields: {})",
                    parts
                        .iter()
                        .map(|p| p.key.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            _ => {}
        }
        out.push_str("```\n");
        match &tab.exchange {
            Exchange::Done(done) => {
                let _ = write!(
                    out,
                    "\nResponse: {} {} · {} · {}\n```\n{}\n```\n",
                    done.status,
                    done.reason,
                    human_time(done.elapsed),
                    human_size(done.size),
                    clip(&done.pretty, 6000)
                );
            }
            Exchange::Failed(error) => {
                let _ = write!(out, "\nSending it failed: {error}\n");
            }
            _ => {}
        }
        let failed: Vec<&api::TestResult> = tab.tests.iter().filter(|t| !t.passed).collect();
        if !failed.is_empty() {
            out.push_str("\nFailing tests:\n");
            for test in failed {
                let _ = writeln!(
                    out,
                    "- {}: {}",
                    test.name,
                    test.error.as_deref().unwrap_or("failed")
                );
            }
        }
        out.push_str("\nCan you help me with this?");
        out
    }

    /// Opens a chat about the request with that text ready to send.
    pub(super) fn ask_agent(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let Some(project) = self.collection(&tab.file).map(|c| c.project.clone()) else {
            return;
        };
        let event = Event::AskAgent {
            project,
            title: format!("API: {} {}", tab.draft.method, tab.draft.name),
            prompt: self.agent_prompt(tab),
        };
        cx.emit(event);
    }

    /// Puts the response text of the chosen tab into the viewer.
    pub(super) fn show_response(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let Exchange::Done(done) = &tab.exchange else {
            return;
        };
        let (text, syntax) = match tab.response_tab {
            ResponseTab::Pretty => (done.pretty.clone(), done.syntax),
            ResponseTab::Raw => (done.raw.clone(), "response.txt"),
            ResponseTab::Headers | ResponseTab::Tests | ResponseTab::Console => return,
        };
        let viewer = tab.viewer.clone();
        viewer.update(cx, |input, cx| {
            input.set_editor_path(syntax.into());
            input.set_text(text, cx);
        });
    }

    pub(super) fn set_response_tab(&mut self, id: u64, kind: ResponseTab, cx: &mut Context<Self>) {
        if let Some(tab) = self.tab_mut(id) {
            tab.response_tab = kind;
        }
        self.show_response(id, cx);
        cx.notify();
    }

    pub(super) fn copy_response(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        if let Exchange::Done(done) = &tab.exchange {
            let text = match tab.response_tab {
                ResponseTab::Raw => done.raw.clone(),
                _ => done.pretty.clone(),
            };
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.notify_user("Copied the response.", cx);
        }
    }

    pub(super) fn copy_curl(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.tab(id) else {
            return;
        };
        let prepared = self.prepare_tab(tab);
        let missing = prepared.missing.clone();
        cx.write_to_clipboard(ClipboardItem::new_string(api::to_curl(&prepared)));
        if missing.is_empty() {
            self.notify_user("Copied as cURL.", cx);
        } else {
            self.notify_user(
                format!(
                    "Copied as cURL. No value for {}.",
                    missing
                        .iter()
                        .map(|name| format!("{{{{{name}}}}}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                cx,
            );
        }
    }
}
