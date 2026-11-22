//! An agent's download into a folder (context.md K50, K51): the page's next
//! download goes into the folder instead of `~/Downloads`, still listed in
//! the page's downloads, and the agent learns where it landed.
use super::*;
type LocalBoxFuture<T> = std::pin::Pin<Box<dyn std::future::Future<Output = T>>>;
use monocode_cef::{Download, DownloadState};

/// The agent waiting for a page's next download.
pub(super) struct DownloadWait {
    dir: PathBuf,
    /// The download, once it started.
    id: Option<u32>,
    done: async_channel::Sender<Result<PathBuf, String>>,
}

impl BrowserPage {
    /// The page's next download goes into `dir`, made if missing; resolves
    /// with the finished file's path, or why it failed or was cancelled.
    /// Waits as long as the download takes: the caller bounds it. A later
    /// call replaces an earlier one still waiting.
    pub(crate) fn expect_download(
        &mut self,
        dir: PathBuf,
    ) -> LocalBoxFuture<Result<PathBuf, String>> {
        if let Err(error) = std::fs::create_dir_all(&dir) {
            let error = format!("could not make {}: {error}", dir.display());
            return Box::pin(async move { Err(error) });
        }
        let (done, result) = async_channel::bounded(1);
        // Chromium sends the next download there while this lives; dropped
        // with the future, a download that has not started goes the usual way.
        let into =
            (self.host.view.borrow().as_ref()).map(|h| h.view.download_next_into(dir.clone()));
        self.download_wait = Some(DownloadWait {
            dir,
            id: None,
            done,
        });
        Box::pin(async move {
            let _into = into;
            result
                .recv()
                .await
                .unwrap_or_else(|_| Err("the page stopped waiting for the download".to_owned()))
        })
    }

    #[cfg(test)]
    pub(crate) fn waits_for_download(&self) -> bool {
        self.download_wait.is_some()
    }

    /// Hands a download's progress to the agent waiting for it, if any;
    /// before [`BrowserPage::downloads`] learns of it.
    pub(super) fn route_download(&mut self, download: &Download) {
        let known = self.downloads.iter().any(|d| d.id == download.id);
        let Some(wait) = &mut self.download_wait else {
            return;
        };
        if wait.done.is_closed() {
            self.download_wait = None;
            return;
        }
        match wait.id {
            Some(id) if id == download.id => {}
            None if !known && download.path.parent() == Some(wait.dir.as_path()) => {
                wait.id = Some(download.id);
            }
            _ => return,
        }
        let result = match download.state {
            DownloadState::InProgress => return,
            DownloadState::Complete => Ok(download.path.clone()),
            DownloadState::Canceled => Err("the download was cancelled".to_owned()),
            DownloadState::Failed => Err("the download failed".to_owned()),
        };
        let _ = wait.done.try_send(result);
        self.download_wait = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use monocode_cef::BrowserEvent;

    fn download(id: u32, path: &Path, state: DownloadState) -> Download {
        Download {
            id,
            path: path.to_owned(),
            received: 4,
            total: Some(4),
            state,
        }
    }

    fn page(cx: &mut gpui::TestAppContext) -> Entity<BrowserPage> {
        cx.new(|cx| BrowserPage::new(String::new(), String::new(), 100, "/tmp".into(), cx))
    }

    fn poll(
        future: &mut LocalBoxFuture<Result<PathBuf, String>>,
    ) -> Option<Result<PathBuf, String>> {
        use std::task::{Context, Poll};
        match future
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
        {
            Poll::Ready(result) => Some(result),
            Poll::Pending => None,
        }
    }

    #[gpui::test]
    fn the_next_download_resolves_with_its_path_and_stays_listed(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let into = dir.path().join("out").join("files");
        let page = page(cx);
        let mut waiting = page.update(cx, |page, _| page.expect_download(into.clone()));
        assert!(into.is_dir());
        let file = into.join("report.pdf");
        page.update(cx, |page, cx| {
            // One already running elsewhere is not it.
            let other = Path::new("/Users/me/Downloads/other.zip");
            page.on_event(
                BrowserEvent::Download(download(1, other, DownloadState::InProgress)),
                cx,
            );
            page.on_event(
                BrowserEvent::Download(download(2, &file, DownloadState::InProgress)),
                cx,
            );
            page.on_event(
                BrowserEvent::Download(download(1, other, DownloadState::Complete)),
                cx,
            );
        });
        assert_eq!(poll(&mut waiting), None);
        page.update(cx, |page, cx| {
            page.on_event(
                BrowserEvent::Download(download(2, &file, DownloadState::Complete)),
                cx,
            );
            assert_eq!(page.downloads.len(), 2);
            assert!(page.download_wait.is_none());
            assert!(!page.downloads_open);
        });
        assert_eq!(poll(&mut waiting), Some(Ok(file)));
    }

    #[gpui::test]
    fn a_cancelled_or_failed_download_resolves_with_why(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let page = page(cx);
        for (state, why) in [
            (DownloadState::Canceled, "the download was cancelled"),
            (DownloadState::Failed, "the download failed"),
        ] {
            let mut waiting = page.update(cx, |page, _| page.expect_download(dir.path().into()));
            let id = page.read_with(cx, |page, _| page.downloads.len() as u32 + 10);
            let file = dir.path().join(format!("f{id}.bin"));
            page.update(cx, |page, cx| {
                page.on_event(
                    BrowserEvent::Download(download(id, &file, DownloadState::InProgress)),
                    cx,
                );
                page.on_event(BrowserEvent::Download(download(id, &file, state)), cx);
            });
            assert_eq!(poll(&mut waiting), Some(Err(why.to_owned())));
        }
    }

    #[gpui::test]
    fn a_caller_that_gave_up_or_was_replaced_lets_go(cx: &mut gpui::TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let page = page(cx);
        let mut first = page.update(cx, |page, _| page.expect_download(dir.path().into()));
        let second = page.update(cx, |page, _| page.expect_download(dir.path().into()));
        assert_eq!(
            poll(&mut first),
            Some(Err("the page stopped waiting for the download".to_owned()))
        );
        // The caller's timeout drops the future: the next download is the
        // user's again.
        drop(second);
        let file = dir.path().join("a.txt");
        page.update(cx, |page, cx| {
            page.on_event(
                BrowserEvent::Download(download(5, &file, DownloadState::InProgress)),
                cx,
            );
            assert!(page.download_wait.is_none());
        });
        // A folder that cannot be made fails at once.
        let blocked = dir.path().join("a.txt").join("sub");
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let mut failed = page.update(cx, |page, _| page.expect_download(blocked));
        assert!(matches!(poll(&mut failed), Some(Err(e)) if e.starts_with("could not make")));
    }
}
