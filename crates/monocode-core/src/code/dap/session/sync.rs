//! A session's side of the breakpoint store: sending a file's breakpoints
//! and the exception filters to the adapter when they change, and writing
//! the adapter's verdict on each breakpoint back into the store.
use super::Session;
use crate::code::dap::breakpoints::{Breakpoint, BreakpointChange, BreakpointStatus};
use crate::code::dap::client::lock;
use serde_json::{json, Value};
use std::path::Path;

impl Session {
    pub(super) fn on_store_change(&self, change: &BreakpointChange) {
        if !lock(&self.cache).configured {
            // Everything is sent when the adapter asks for it.
            return;
        }
        match change {
            BreakpointChange::File(path) => {
                let path = path.clone();
                self.background("set breakpoints", move |session| {
                    session.sync_file(&path);
                    Ok(())
                });
            }
            BreakpointChange::Exceptions => self.background("set breakpoints", |session| {
                session.sync_exceptions();
                Ok(())
            }),
            BreakpointChange::Status(_) => {}
        }
    }

    pub(super) fn sync_file(&self, path: &Path) {
        let _order = lock(&self.sync);
        let breakpoints: Vec<Breakpoint> = self
            .store
            .in_file(path)
            .into_iter()
            .filter(|breakpoint| breakpoint.enabled)
            .collect();
        {
            let mut cache = lock(&self.cache);
            if cache.state.is_terminated() {
                return;
            }
            if breakpoints.is_empty() {
                if !cache.sent_files.remove(path) {
                    return;
                }
            } else {
                cache.sent_files.insert(path.to_path_buf());
            }
        }
        let entries: Vec<Value> = breakpoints
            .iter()
            .map(|breakpoint| {
                let mut entry = json!({ "line": breakpoint.line });
                let optional = [
                    (
                        "condition",
                        "supportsConditionalBreakpoints",
                        &breakpoint.condition,
                    ),
                    (
                        "hitCondition",
                        "supportsHitConditionalBreakpoints",
                        &breakpoint.hit_condition,
                    ),
                    ("logMessage", "supportsLogPoints", &breakpoint.log_message),
                ];
                for (key, capability, value) in optional {
                    if let (Some(value), true) = (value, self.supports(capability)) {
                        entry[key] = json!(value);
                    }
                }
                entry
            })
            .collect();
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let result = self.ask(
            "setBreakpoints",
            json!({
                "source": { "path": path, "name": name },
                "breakpoints": entries,
                "lines": breakpoints.iter().map(|breakpoint| breakpoint.line).collect::<Vec<_>>(),
                "sourceModified": false,
            }),
        );
        let answers: Vec<(u64, BreakpointStatus)> = match result {
            Ok(body) => {
                let answered = body.get("breakpoints").and_then(Value::as_array);
                let mut cache = lock(&self.cache);
                breakpoints
                    .iter()
                    .zip(answered.into_iter().flatten())
                    .map(|(breakpoint, answer)| {
                        if let Some(id) = answer.get("id").and_then(Value::as_i64) {
                            cache
                                .adapter_breakpoints
                                .insert(id, (path.to_path_buf(), breakpoint.id));
                        }
                        (breakpoint.id, status_of(answer, breakpoint.line))
                    })
                    .collect()
            }
            Err(error) => breakpoints
                .iter()
                .map(|breakpoint| {
                    let message = Some(error.to_string());
                    (breakpoint.id, BreakpointStatus::Rejected { message })
                })
                .collect(),
        };
        self.store.report(path, &answers);
    }

    pub(super) fn sync_exceptions(&self) {
        let _order = lock(&self.sync);
        let offered: Vec<String> = lock(&self.capabilities)
            .get("exceptionBreakpointFilters")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|filter| Some(filter.get("filter")?.as_str()?.to_string()))
            .collect();
        if offered.is_empty() {
            return;
        }
        let mut filters = self.adapter.exception_filters(&self.store.exceptions());
        filters.retain(|filter| offered.contains(filter));
        // A refusal leaves the adapter's previous filters; nothing to mark.
        self.ask("setExceptionBreakpoints", json!({ "filters": filters }))
            .ok();
    }

    /// The adapter changed its mind about a breakpoint (the module it is in
    /// was loaded, say).
    pub(super) fn on_breakpoint(&self, breakpoint: &Value) {
        let Some(id) = breakpoint.get("id").and_then(Value::as_i64) else {
            return;
        };
        let Some((path, store_id)) = lock(&self.cache).adapter_breakpoints.get(&id).cloned() else {
            return;
        };
        let line = self
            .store
            .in_file(&path)
            .iter()
            .find(|known| known.id == store_id)
            .map(|known| known.line);
        if let Some(line) = line {
            self.store
                .report(&path, &[(store_id, status_of(breakpoint, line))]);
        }
    }
}

fn status_of(answer: &Value, asked_line: u32) -> BreakpointStatus {
    if answer.get("verified").and_then(Value::as_bool) == Some(true) {
        let line = answer
            .get("line")
            .and_then(Value::as_u64)
            .and_then(|line| u32::try_from(line).ok())
            .unwrap_or(asked_line);
        BreakpointStatus::Verified { line }
    } else {
        let message = answer
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string);
        BreakpointStatus::Rejected { message }
    }
}
