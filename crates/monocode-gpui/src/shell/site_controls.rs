//! The shell's part of a page's Site Control Center and Capture (docs/
//! native/modes.md §Site Control Center, §Capture): a capture saved into the
//! project's folder or sent to a chat as a grab is (K49), Settings → Browser,
//! and the Command Bar's Capture, Site Controls and Clear … and Refresh.
use super::*;
use crate::browser::{CaptureTo, Captured, ClearSiteData, SiteAsk};
use std::path::{Path, PathBuf};

/// The folder in a project captures are saved into.
const CAPTURES: &str = "captures";

/// A capture's file name: the page's host and when it was taken.
pub(super) fn capture_file_name(url: &str, at: chrono::DateTime<chrono::Local>) -> String {
    let host = monocode_core::browser::sites::host_of(url);
    let host: String = (host.chars())
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let host = host.trim_matches(['-', '.']);
    let host = if host.is_empty() { "page" } else { host };
    format!("capture-{host}-{}.png", at.format("%Y%m%d-%H%M%S"))
}

/// Writes `png` as `name` into `dir` (made if missing), never over another
/// file: the path written.
fn write_capture(dir: &Path, name: &str, png: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stem = name.trim_end_matches(".png");
    let mut path = dir.join(name);
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.png"));
        n += 1;
    }
    std::fs::write(&path, png).map_err(|e| e.to_string())?;
    Ok(path)
}

impl Shell {
    /// What a page's Site Control Center or Capture asks of the shell.
    pub(super) fn browse_site_ask(
        &mut self,
        pane: i64,
        ask: SiteAsk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ask {
            SiteAsk::Capture(shot, CaptureTo::Project) => self.save_capture(pane, shot, cx),
            SiteAsk::Capture(shot, CaptureTo::Chat) => self.send_capture(pane, shot, cx),
            SiteAsk::BrowserSettings => {
                self.open_settings(crate::settings::Section::Browser, window, cx)
            }
        }
    }

    /// The project a page belongs to: its owner's, else the one in front.
    fn capture_project(&self, pane: i64) -> Option<PathBuf> {
        let state = self.workspace.browser_panes.get(&pane);
        let owned = state.and_then(|state| {
            (self.session_summaries.get(&state.owner))
                .map(|owner| owner.cwd.clone())
                .or_else(|| state.project.clone())
        });
        let current = (!self.workspace.project.is_empty()).then(|| self.workspace.project.clone());
        owned
            .or(current)
            .or_else(|| self.workspace.projects.first().cloned())
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
    }

    fn capture_notice(&mut self, message: String, cx: &mut Context<Self>) {
        self.notification_state
            .browser_grab
            .update(cx, |notice, cx| notice.show(message, cx));
        cx.notify();
    }

    /// Save to Project: `captures/` in the page's project.
    fn save_capture(&mut self, pane: i64, shot: Captured, cx: &mut Context<Self>) {
        let Some(project) = self.capture_project(pane) else {
            return self.capture_notice(
                "There is no project folder to save the capture in.".into(),
                cx,
            );
        };
        let name = capture_file_name(&shot.url, chrono::Local::now());
        let dir = project.join(CAPTURES);
        let work = cx
            .background_executor()
            .spawn(async move { write_capture(&dir, &name, &shot.png) });
        cx.spawn(async move |this, cx| {
            let written = work.await;
            let _ = this.update(cx, |this, cx| {
                let message = match written {
                    Ok(path) => {
                        let shown = path.strip_prefix(&project).unwrap_or(&path);
                        format!("Capture saved to {}", shown.display())
                    }
                    Err(error) => format!("The capture could not be saved: {error}"),
                };
                this.capture_notice(message, cx);
            });
        })
        .detach();
    }

    /// Send to Chat: an image attachment in the chat a grab would go to.
    fn send_capture(&mut self, pane: i64, shot: Captured, cx: &mut Context<Self>) {
        let Some((id, view)) = (self.browser_grab_target(pane))
            .and_then(|id| Some((id, self.ensure_session(id, cx)?)))
        else {
            return self.capture_notice(
                "There is no chat to send it to. Open a chat, then capture again.".into(),
                cx,
            );
        };
        let name = capture_file_name(&shot.url, chrono::Local::now());
        let store = self.store.clone();
        let work = cx.background_executor().spawn(async move {
            // Kept with the conversation, as pasted images are.
            let dir = locked(&store)?
                .thread_attachments_dir(id)?
                .unwrap_or_else(|| std::env::temp_dir().join("monocode-attachments"));
            write_capture(&dir, &name, &shot.png)
        });
        cx.spawn(async move |this, cx| {
            let written = work.await;
            let _ = this.update(cx, |this, cx| match written {
                Ok(path) => {
                    view.update(cx, |chat, cx| chat.attach_files(vec![path], cx));
                    this.pending_chat_reveal = Some(id);
                    this.pending_composer_focus = Some(id);
                    cx.notify();
                }
                Err(error) => this.capture_notice(error, cx),
            });
        })
        .detach();
    }

    /// The Command Bar's Capture: on the page on screen.
    pub(super) fn browse_capture(&mut self, cx: &mut Context<Self>) {
        if let Some(page) =
            (self.workspace.browse.active).and_then(|pane| self.browser_page(pane, cx))
        {
            page.update(cx, |page, cx| {
                page.start_capture(cx);
            });
        }
    }

    /// The Command Bar's Site Controls: the panel under the address field.
    pub(super) fn browse_site_controls(&mut self, cx: &mut Context<Self>) {
        if let Some(page) =
            (self.workspace.browse.active).and_then(|pane| self.browser_page(pane, cx))
        {
            page.update(cx, |page, cx| {
                if !page.site_controls_open() {
                    page.toggle_site_controls(cx);
                }
            });
        }
    }

    /// The Command Bar's Clear Cookies and Refresh, Clear Cache and Refresh.
    pub(super) fn browse_clear_site(&mut self, kind: ClearSiteData, cx: &mut Context<Self>) {
        if let Some(page) =
            (self.workspace.browse.active).and_then(|pane| self.browser_page(pane, cx))
        {
            page.update(cx, |page, cx| {
                page.clear_site_data(kind, cx);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_is_named_after_its_site_and_time_and_never_overwrites() {
        use chrono::TimeZone;
        let at = chrono::Local
            .with_ymd_and_hms(2026, 11, 25, 9, 5, 7)
            .unwrap();
        assert_eq!(
            capture_file_name("https://www.example.com:8080/a?b", at),
            "capture-www.example.com-20261125-090507.png"
        );
        assert_eq!(
            capture_file_name("about:blank", at),
            "capture-page-20261125-090507.png"
        );
        let dir = tempfile::tempdir().unwrap();
        let first = write_capture(dir.path(), "c.png", b"1").unwrap();
        let second = write_capture(dir.path(), "c.png", b"2").unwrap();
        assert_eq!(first, dir.path().join("c.png"));
        assert_eq!(second, dir.path().join("c-2.png"));
        assert_eq!(std::fs::read(&first).unwrap(), b"1");
    }
}
