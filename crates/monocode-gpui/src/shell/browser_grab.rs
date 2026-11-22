//! A grabbed browser element reaches a chat composer (context.md K49): the
//! session that opened the page if an agent did, else the focused chat.
//! It arrives as two attachments, like pasted images: the element as a
//! Markdown file the prompt carries inline, and its crop with an
//! `[Image #N]` marker. With no chat to take it, a notice says so.
use super::*;
use monocode_core::browser::grab::Grab;
use std::rc::Rc;

/// Where a grab goes: the agent session that opened the page, else the
/// focused chat.
fn grab_target(opened_by: Option<i64>, focused: Option<i64>) -> Option<i64> {
    opened_by.or(focused)
}

/// What gets written for a grab: the Markdown and the crop, named after
/// the element.
struct GrabFiles {
    stem: String,
    markdown: String,
    image: Option<(&'static str, Vec<u8>)>,
}

impl GrabFiles {
    fn of(grab: &Grab) -> Self {
        GrabFiles {
            stem: grab.file_stem(),
            markdown: grab.markdown(),
            image: (grab.image.as_ref()).map(|image| (image.extension(), image.bytes.clone())),
        }
    }

    /// Writes them into `dir`; the Markdown first, then the image.
    fn write(self, dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let unique = &monocode_core::mono::new_id()[..8];
        let stem = format!("{}-{unique}", self.stem);
        let text = dir.join(format!("{stem}.md"));
        std::fs::write(&text, self.markdown).map_err(|e| e.to_string())?;
        let mut paths = vec![text];
        if let Some((extension, bytes)) = self.image {
            let image = dir.join(format!("{stem}.{extension}"));
            std::fs::write(&image, bytes).map_err(|e| e.to_string())?;
            paths.push(image);
        }
        Ok(paths)
    }
}

impl Shell {
    /// The chat a grab from the page of `pane` goes to.
    pub(super) fn browser_grab_target(&self, pane: i64) -> Option<i64> {
        let known = |id: &i64| self.session_summaries.contains_key(id);
        let opened_by = (self.workspace.browser_panes.get(&pane))
            .and_then(|state| state.opened_by)
            .filter(known);
        grab_target(opened_by, self.terminal_owner().filter(known))
    }

    fn browser_grab_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.notification_state
            .browser_grab
            .update(cx, |notice, cx| notice.show(message, cx));
        cx.notify();
    }

    /// Puts a grab from the page of `pane` in its chat's composer.
    pub(super) fn deliver_browser_grab(
        &mut self,
        pane: i64,
        result: Result<Rc<Grab>, String>,
        cx: &mut Context<Self>,
    ) {
        let grab = match result {
            Ok(grab) => grab,
            Err(error) => {
                return self
                    .browser_grab_failed(format!("The element could not be read: {error}"), cx);
            }
        };
        let Some((id, view)) = (self.browser_grab_target(pane))
            .and_then(|id| Some((id, self.ensure_session(id, cx)?)))
        else {
            return self.browser_grab_failed(
                "There is no chat to send it to. Open a chat, then grab the element again.".into(),
                cx,
            );
        };
        let files = GrabFiles::of(&grab);
        let store = self.store.clone();
        let work = cx.background_executor().spawn(async move {
            // Kept with the conversation, as pasted images are.
            let dir = locked(&store)?
                .thread_attachments_dir(id)?
                .unwrap_or_else(|| std::env::temp_dir().join("monocode-attachments"));
            files.write(&dir)
        });
        cx.spawn(async move |this, cx| {
            let written = work.await;
            let _ = this.update(cx, |this, cx| match written {
                Ok(paths) => {
                    view.update(cx, |chat, cx| chat.attach_files(paths, cx));
                    this.pending_chat_reveal = Some(id);
                    this.pending_composer_focus = Some(id);
                    cx.notify();
                }
                Err(error) => this.browser_grab_failed(error, cx),
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_core::browser::grab::{Element, Image};

    fn grab() -> Rc<Grab> {
        Rc::new(Grab {
            element: Element {
                tag: "button".into(),
                text: "Save".into(),
                selector: "button.save".into(),
                ..Element::default()
            },
            image: Some(Image {
                mime: "image/png",
                bytes: vec![0x89, b'P', b'N', b'G'],
            }),
        })
    }

    fn settle(cx: &mut gpui::TestAppContext) {
        for _ in 0..4 {
            cx.refresh().unwrap();
            cx.run_until_parked();
        }
    }

    #[test]
    fn a_grab_goes_to_the_agent_that_opened_the_page_else_the_focused_chat() {
        assert_eq!(grab_target(Some(4), Some(9)), Some(4));
        assert_eq!(grab_target(None, Some(9)), Some(9));
        assert_eq!(grab_target(None, None), None);
    }

    #[gpui::test]
    fn a_grab_lands_in_the_opening_agents_composer_as_text_and_image(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| cx.set_reduce_motion(true));
        let (window, other, session) = super::super::tests::window(cx);
        let (agent_page, user_page) = window
            .update(cx, |shell, window, cx| {
                shell.open(session, window, cx);
                shell.new_browser_pane(Some("http://localhost:5173/".into()), window, cx);
                let user_page = shell.workspace.active.unwrap();
                // `other` opened this one, from `session`'s window position.
                let agent_page =
                    shell.open_agent_browser_page(other, "https://example.com/".into(), false, cx);
                assert_eq!(shell.browser_grab_target(user_page), Some(session));
                assert_eq!(shell.browser_grab_target(agent_page), Some(other));
                (agent_page, user_page)
            })
            .unwrap();
        window
            .update(cx, |shell, _, cx| {
                shell.deliver_browser_grab(agent_page, Ok(grab()), cx)
            })
            .unwrap();
        settle(cx);
        window
            .update(cx, |shell, _, cx| {
                let chat = shell.sessions[&other].read(cx);
                let files = chat.draft_attachments().to_vec();
                assert_eq!(files.len(), 2, "{files:?}");
                assert_eq!(files[0].extension().unwrap(), "md");
                assert_eq!(files[1].extension().unwrap(), "png");
                let name = files[0].file_name().unwrap().to_string_lossy();
                assert!(name.starts_with("browser-button-save-"), "{name}");
                let text = std::fs::read_to_string(&files[0]).unwrap();
                assert!(text.contains("- Selector: `button.save`"), "{text}");
                let draft = chat.input.read(cx).content.to_string();
                assert!(draft.contains("[Image #1]"), "{draft}");
                assert!(
                    shell.sessions[&session]
                        .read(cx)
                        .draft_attachments()
                        .is_empty(),
                    "not the focused chat's"
                );
                assert_eq!(shell.workspace.session_for_pane(user_page), session);
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_grab_with_no_chat_to_take_it_says_so(cx: &mut gpui::TestAppContext) {
        let (window, _, _) = super::super::tests::window(cx);
        window
            .update(cx, |shell, _, cx| {
                // A pane no session owns, with nothing focused.
                shell.workspace.active = None;
                shell.workspace.project_focus.clear();
                shell.deliver_browser_grab(-12345, Ok(grab()), cx);
                let notice = shell.notification_state.browser_grab.read(cx);
                assert!(notice.message().unwrap().contains("no chat"));

                shell.deliver_browser_grab(-12345, Err("the page is gone".into()), cx);
                let notice = shell.notification_state.browser_grab.read(cx);
                assert!(notice.message().unwrap().contains("the page is gone"));
            })
            .unwrap();
    }
}
