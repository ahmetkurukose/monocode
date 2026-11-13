//! What a page asks of its browser beyond showing itself: a new window, an
//! alert, a file to upload, a name and password, a place for a download.
//!
//! The questions are asked in AppKit sheets on the window, not in GPUI: a
//! sheet is a window of its own, so it is above the page without the page
//! having to give way to a picture of itself.
use super::host::Link;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained,
    runtime::Bool,
};
use objc2_app_kit::{NSAlert, NSOpenPanel, NSSecureTextField, NSTextField, NSView};
use objc2_foundation::{
    NSArray, NSData, NSError, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
    NSURLAuthenticationChallenge, NSURLCredential, NSURLCredentialPersistence, NSURLResponse,
    NSURLSessionAuthChallengeDisposition,
};
use objc2_web_kit::{
    WKDownload, WKDownloadDelegate, WKFrameInfo, WKNavigationAction, WKOpenPanelParameters,
    WKUIDelegate, WKWebView, WKWebViewConfiguration, WKWindowFeatures,
};
use std::{cell::RefCell, collections::HashMap, path::PathBuf};

/// `NSAlertFirstButtonReturn` and `NSModalResponseOK`.
const FIRST_BUTTON: isize = 1000;
const PANEL_OK: isize = 1;

fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

/// One text field of a sheet: its placeholder and whether it hides typing.
struct Field {
    placeholder: &'static str,
    secret: bool,
    text: Option<String>,
}

/// A sheet on the page's window with a message, buttons (the first is the
/// default) and optional fields. `done` gets whether the first button was
/// pressed and what the fields held. Without a window it is declined.
fn sheet(
    web: &WKWebView,
    title: &str,
    detail: &str,
    buttons: &[&str],
    fields: Vec<Field>,
    done: impl FnOnce(bool, Vec<String>) + 'static,
) {
    let (Some(window), Some(mtm)) = (web.window(), MainThreadMarker::new()) else {
        done(false, Vec::new());
        return;
    };
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(title));
    alert.setInformativeText(&ns(detail));
    for button in buttons {
        alert.addButtonWithTitle(&ns(button));
    }
    let mut inputs: Vec<Retained<NSTextField>> = Vec::new();
    if !fields.is_empty() {
        let row = 28.;
        let height = row * fields.len() as f64;
        let frame = NSRect::new(NSPoint::new(0., 0.), NSSize::new(280., height));
        let holder = NSView::initWithFrame(NSView::alloc(mtm), frame);
        for (ix, field) in fields.iter().enumerate() {
            // AppKit's y runs upwards: the first field goes on top.
            let frame = NSRect::new(
                NSPoint::new(0., height - row * (ix as f64 + 1.) + 2.),
                NSSize::new(280., 24.),
            );
            let input: Retained<NSTextField> = if field.secret {
                Retained::into_super(NSSecureTextField::initWithFrame(
                    NSSecureTextField::alloc(mtm),
                    frame,
                ))
            } else {
                NSTextField::initWithFrame(NSTextField::alloc(mtm), frame)
            };
            input.setPlaceholderString(Some(&ns(field.placeholder)));
            if let Some(text) = &field.text {
                input.setStringValue(&ns(text));
            }
            holder.addSubview(&input);
            inputs.push(input);
        }
        alert.setAccessoryView(Some(&holder));
    }
    let done = std::cell::Cell::new(Some(done));
    let block = block2::RcBlock::new(move |response: isize| {
        if let Some(done) = done.take() {
            let texts = (inputs.iter())
                .map(|input| input.stringValue().to_string())
                .collect();
            done(response == FIRST_BUTTON, texts);
        }
    });
    alert.beginSheetModalForWindow_completionHandler(&window, Some(&block));
}

fn page_name(web: &WKWebView) -> String {
    let url = unsafe { web.URL() }
        .and_then(|url| url.absoluteString())
        .map(|url| url.to_string())
        .unwrap_or_default();
    monocode_core::browser::label(&url)
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MonocodeBrowserUi"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Link]
    pub(super) struct Ui;
    unsafe impl NSObjectProtocol for Ui {}
    unsafe impl WKUIDelegate for Ui {
        /// A link or a script asked for a new window: it opens as a page of
        /// its own. No view is handed back, so the new page is not tied to
        /// the one that opened it (`window.opener` is empty there).
        #[unsafe(method_id(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:))]
        unsafe fn create(
            &self,
            _: &WKWebView,
            _: &WKWebViewConfiguration,
            action: &WKNavigationAction,
            _: &WKWindowFeatures,
        ) -> Option<Retained<WKWebView>> {
            let url = unsafe { action.request().URL() }
                .and_then(|url| url.absoluteString())
                .map(|url| url.to_string());
            if let Some(url) = url.filter(|url| !url.is_empty()) {
                self.ivars().later(move |view, cx| view.open_new(url, cx));
            }
            None
        }

        #[unsafe(method(webView:runJavaScriptAlertPanelWithMessage:initiatedByFrame:completionHandler:))]
        unsafe fn alert(
            &self,
            web: &WKWebView,
            message: &NSString,
            _: &WKFrameInfo,
            done: &block2::DynBlock<dyn Fn()>,
        ) {
            let done = done.copy();
            let title = format!("{} says", page_name(web));
            sheet(
                web,
                &title,
                &message.to_string(),
                &["OK"],
                vec![],
                move |_, _| done.call(()),
            );
        }

        #[unsafe(method(webView:runJavaScriptConfirmPanelWithMessage:initiatedByFrame:completionHandler:))]
        unsafe fn confirm(
            &self,
            web: &WKWebView,
            message: &NSString,
            _: &WKFrameInfo,
            done: &block2::DynBlock<dyn Fn(Bool)>,
        ) {
            let done = done.copy();
            let title = format!("{} asks", page_name(web));
            sheet(
                web,
                &title,
                &message.to_string(),
                &["OK", "Cancel"],
                vec![],
                move |ok, _| done.call((Bool::new(ok),)),
            );
        }

        #[unsafe(method(webView:runJavaScriptTextInputPanelWithPrompt:defaultText:initiatedByFrame:completionHandler:))]
        unsafe fn prompt(
            &self,
            web: &WKWebView,
            prompt: &NSString,
            default: Option<&NSString>,
            _: &WKFrameInfo,
            done: &block2::DynBlock<dyn Fn(*mut NSString)>,
        ) {
            let done = done.copy();
            let title = format!("{} asks", page_name(web));
            let field = Field {
                placeholder: "",
                secret: false,
                text: default.map(|text| text.to_string()),
            };
            sheet(
                web,
                &title,
                &prompt.to_string(),
                &["OK", "Cancel"],
                vec![field],
                move |ok, texts| match texts.first().filter(|_| ok) {
                    Some(text) => done.call((Retained::as_ptr(&ns(text)).cast_mut(),)),
                    None => done.call((std::ptr::null_mut(),)),
                },
            );
        }

        /// `<input type=file>`: the system's own panel.
        #[unsafe(method(webView:runOpenPanelWithParameters:initiatedByFrame:completionHandler:))]
        unsafe fn open_panel(
            &self,
            web: &WKWebView,
            parameters: &WKOpenPanelParameters,
            _: &WKFrameInfo,
            done: &block2::DynBlock<dyn Fn(*mut NSArray<NSURL>)>,
        ) {
            let done = done.copy();
            let (Some(window), Some(mtm)) = (web.window(), MainThreadMarker::new()) else {
                done.call((std::ptr::null_mut(),));
                return;
            };
            let panel = NSOpenPanel::openPanel(mtm);
            panel.setCanChooseFiles(true);
            panel.setAllowsMultipleSelection(unsafe { parameters.allowsMultipleSelection() });
            panel.setCanChooseDirectories(unsafe { parameters.allowsDirectories() });
            let chosen = panel.clone();
            let block = block2::RcBlock::new(move |response: isize| {
                if response == PANEL_OK {
                    let urls = chosen.URLs();
                    done.call((Retained::as_ptr(&urls).cast_mut(),));
                } else {
                    done.call((std::ptr::null_mut(),));
                }
            });
            panel.beginSheetModalForWindow_completionHandler(&window, &block);
        }
    }
);

impl Ui {
    pub(super) fn new(mtm: MainThreadMarker, link: Link) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(link);
        unsafe { msg_send![super(this), init] }
    }
}

/// Where downloads go, and the files in flight.
pub(super) struct Shelf {
    link: Link,
    files: RefCell<HashMap<usize, PathBuf>>,
}

fn downloads_folder() -> PathBuf {
    if let Some(folder) = std::env::var_os("MONOCODE_BROWSER_DOWNLOADS") {
        return folder.into();
    }
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join("Downloads")
}

/// `name` in `folder`, or `name 2`, `name 3`… when it is taken.
pub(super) fn free_name(folder: &std::path::Path, name: &str) -> PathBuf {
    let name = name.replace(['/', ':'], "_");
    let name = if name.trim().is_empty() {
        "download".to_owned()
    } else {
        name
    };
    let first = folder.join(&name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (name.clone(), String::new()),
    };
    (2..10_000)
        .map(|n| folder.join(format!("{stem} {n}{extension}")))
        .find(|path| !path.exists())
        .unwrap_or(first)
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "MonocodeBrowserDownloads"]
    #[thread_kind = MainThreadOnly]
    #[ivars = Shelf]
    pub(super) struct Downloads;
    unsafe impl NSObjectProtocol for Downloads {}
    unsafe impl WKDownloadDelegate for Downloads {
        #[unsafe(method(download:decideDestinationUsingResponse:suggestedFilename:completionHandler:))]
        unsafe fn destination(
            &self,
            download: &WKDownload,
            _: &NSURLResponse,
            suggested: &NSString,
            done: &block2::DynBlock<dyn Fn(*mut NSURL)>,
        ) {
            let folder = downloads_folder();
            let _ = std::fs::create_dir_all(&folder);
            let path = free_name(&folder, &suggested.to_string());
            let url = NSURL::fileURLWithPath(&ns(&path.to_string_lossy()));
            self.ivars()
                .files
                .borrow_mut()
                .insert(download as *const WKDownload as usize, path);
            done.call((Retained::as_ptr(&url).cast_mut(),));
        }

        #[unsafe(method(downloadDidFinish:))]
        unsafe fn finished(&self, download: &WKDownload) {
            let path =
                (self.ivars().files.borrow_mut()).remove(&(download as *const WKDownload as usize));
            if let Some(path) = path {
                self.ivars()
                    .link
                    .later(move |view, cx| view.downloaded(path, cx));
            }
        }

        #[unsafe(method(download:didFailWithError:resumeData:))]
        unsafe fn failed(&self, download: &WKDownload, error: &NSError, _: Option<&NSData>) {
            (self.ivars().files.borrow_mut()).remove(&(download as *const WKDownload as usize));
            let message = error.localizedDescription().to_string();
            self.ivars()
                .link
                .later(move |view, cx| view.download_failed(message, cx));
        }
    }
);

impl Downloads {
    pub(super) fn new(mtm: MainThreadMarker, link: Link) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Shelf {
            link,
            files: RefCell::new(HashMap::new()),
        });
        unsafe { msg_send![super(this), init] }
    }
}

/// An opaque `SecTrustRef`, only ever passed back to AppKit.
#[repr(C)]
struct SecTrust {
    _private: [u8; 0],
}
unsafe impl objc2::RefEncode for SecTrust {
    const ENCODING_REF: objc2::Encoding =
        objc2::Encoding::Pointer(&objc2::Encoding::Struct("__SecTrust", &[]));
}

type Verdict = block2::DynBlock<dyn Fn(NSURLSessionAuthChallengeDisposition, *mut NSURLCredential)>;

/// A server asked who is there, or showed a certificate.
///
/// - A certificate from this machine or its network (localhost, `.local`,
///   private addresses) is accepted as it is: development servers sign their
///   own. Any other certificate is judged by the system as usual.
/// - A name and password (HTTP Basic or Digest) are asked for in a sheet and
///   kept for this run of the app.
pub(super) fn answer_challenge(
    web: &WKWebView,
    challenge: &NSURLAuthenticationChallenge,
    done: &Verdict,
) {
    let space = challenge.protectionSpace();
    let method = space.authenticationMethod().to_string();
    let host = space.host().to_string();
    let default = || {
        done.call((
            NSURLSessionAuthChallengeDisposition::PerformDefaultHandling,
            std::ptr::null_mut(),
        ))
    };
    if method == "NSURLAuthenticationMethodServerTrust" {
        if monocode_core::browser::private_host(&host) {
            let trust: *mut SecTrust = unsafe { msg_send![&*space, serverTrust] };
            if !trust.is_null() {
                let credential: Option<Retained<NSURLCredential>> =
                    unsafe { msg_send![objc2::class!(NSURLCredential), credentialForTrust: trust] };
                if let Some(credential) = credential {
                    done.call((
                        NSURLSessionAuthChallengeDisposition::UseCredential,
                        Retained::as_ptr(&credential).cast_mut(),
                    ));
                    return;
                }
            }
        }
        default();
        return;
    }
    let asks_password = matches!(
        method.as_str(),
        "NSURLAuthenticationMethodHTTPBasic"
            | "NSURLAuthenticationMethodHTTPDigest"
            | "NSURLAuthenticationMethodDefault"
    );
    if !asks_password {
        default();
        return;
    }
    let done = done.copy();
    let detail = match space.realm() {
        Some(realm) if !realm.to_string().is_empty() => format!("{host} — {realm}"),
        _ => host.clone(),
    };
    let fields = vec![
        Field {
            placeholder: "Name",
            secret: false,
            text: None,
        },
        Field {
            placeholder: "Password",
            secret: true,
            text: None,
        },
    ];
    sheet(
        web,
        "Sign in",
        &detail,
        &["Sign In", "Cancel"],
        fields,
        move |ok, texts| {
            if !ok || texts.len() < 2 {
                done.call((
                    NSURLSessionAuthChallengeDisposition::CancelAuthenticationChallenge,
                    std::ptr::null_mut(),
                ));
                return;
            }
            let credential = NSURLCredential::credentialWithUser_password_persistence(
                &ns(&texts[0]),
                &ns(&texts[1]),
                NSURLCredentialPersistence::ForSession,
            );
            done.call((
                NSURLSessionAuthChallengeDisposition::UseCredential,
                Retained::as_ptr(&credential).cast_mut(),
            ));
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_never_overwrites_a_file_that_is_there() {
        let folder = tempfile::tempdir().unwrap();
        let at = |name: &str| free_name(folder.path(), name);
        assert_eq!(at("report.pdf"), folder.path().join("report.pdf"));
        std::fs::write(folder.path().join("report.pdf"), "x").unwrap();
        assert_eq!(at("report.pdf"), folder.path().join("report 2.pdf"));
        std::fs::write(folder.path().join("report 2.pdf"), "x").unwrap();
        assert_eq!(at("report.pdf"), folder.path().join("report 3.pdf"));
        std::fs::write(folder.path().join("LICENSE"), "x").unwrap();
        assert_eq!(at("LICENSE"), folder.path().join("LICENSE 2"));
        // A name cannot reach outside the folder.
        assert_eq!(
            at("../../etc/passwd"),
            folder.path().join(".._.._etc_passwd")
        );
        assert_eq!(at(""), folder.path().join("download"));
    }
}
