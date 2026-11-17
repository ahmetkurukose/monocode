//! What can be changed on a running manager, and the per-server view of a
//! document's diagnostics the editor needs to place them on the right text.
use super::*;

impl Manager {
    /// Replaces what the user set for `project` (the path documents of it
    /// are opened with as `DocumentContext::project`). Documents already
    /// open and servers already running keep what they started with: close
    /// and reopen the documents, and stop the servers, to apply it to them.
    pub fn set_project_settings(&self, project: &Path, settings: ProjectSettings) {
        let mut state = self.shared.state();
        if settings == ProjectSettings::default() {
            state.projects.remove(project);
        } else {
            state.projects.insert(project.to_path_buf(), settings);
        }
    }

    pub fn project_settings(&self, project: &Path) -> ProjectSettings {
        let state = self.shared.state();
        state.projects.get(project).cloned().unwrap_or_default()
    }

    /// Changes how long an unused server lives and how many may be alive,
    /// for the whole app. `None` returns to the options the manager was
    /// made with.
    pub fn set_limits(&self, limits: Option<(Duration, usize)>) {
        let mut state = self.shared.state();
        state.limits = limits;
        self.shared.enforce_cap(&mut state, None);
    }

    /// What each server of an open document says about it, with the version
    /// of the text each report belongs to, so a report on a text that has
    /// since changed is not laid over the new one. A server that is not
    /// running counts as having nothing to say about the current text.
    /// `None` when the document is not open. Never waits.
    pub fn document_diagnostics(&self, path: &Path) -> Option<DocumentDiagnostics> {
        let state = self.shared.state();
        let document = state.documents.get(path)?;
        let servers = document
            .servers
            .iter()
            .filter_map(|id| {
                let entry = state.entries.get(id)?;
                let items = state
                    .diagnostics
                    .get(path)
                    .and_then(|sets| sets.iter().find(|(server, _)| server == id))
                    .map(|(_, found)| found.clone())
                    .unwrap_or_default();
                let version = match entry.running() {
                    Some(_) => entry.reported.get(path).copied(),
                    None => Some(document.version),
                };
                Some(ServerDiagnostics {
                    server: entry.spec.id.to_string(),
                    version,
                    items,
                })
            })
            .collect();
        Some(DocumentDiagnostics {
            version: document.version,
            servers,
        })
    }
}
