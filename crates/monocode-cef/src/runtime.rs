//! Loading and initializing CEF in the app (the browser process).
//!
//! CEF starts lazily, the first time a browser opens. Its framework ships in
//! the app bundle (`Contents/Frameworks`): Chromium's macOS sandbox lets the
//! child processes read only inside the running app's bundle. Everything here
//! runs on the main thread, which on macOS is CEF's UI thread.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    time::{Duration, Instant},
};

use cef::*;

/// The command-line switch through which child processes learn where the
/// framework is, so that they need not derive it from their own path.
pub const FRAMEWORK_SWITCH: &str = "monocode-cef-framework";

/// How to start CEF.
#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    /// `Chromium Embedded Framework.framework`, in the app bundle.
    pub framework_dir: PathBuf,
    /// The helper executable for child processes. On macOS the main helper,
    /// `…/Monocode Helper.app/Contents/MacOS/Monocode Helper`; the GPU,
    /// Renderer, Plugin and Alerts variants sit beside it.
    pub helper: PathBuf,
    /// Root of all browser profiles (`root_cache_path`).
    pub cache_root: PathBuf,
    /// Encrypt cookies with a fixed key instead of the OS keychain. For
    /// development builds, whose signature changes on every build and would
    /// otherwise ask for keychain access each time.
    pub mock_keychain: bool,
    /// UI locale, e.g. `en-US`. Empty: CEF's default.
    pub locale: String,
    /// Where CEF writes its log. `None`: no log file.
    pub log_file: Option<PathBuf>,
}

impl RuntimeConfig {
    /// The library `cef_load_library` is given.
    pub fn library_path(&self) -> PathBuf {
        self.framework_dir.join("Chromium Embedded Framework")
    }

    /// The default profile's folder, inside `cache_root`.
    pub fn default_profile(&self) -> PathBuf {
        self.profiles().join("default")
    }

    /// Where every profile lives: the default one and the projects'
    /// isolated ones ([`crate::Profile`]).
    pub fn profiles(&self) -> PathBuf {
        self.cache_root.join("Profiles")
    }
}

#[derive(Debug)]
pub enum StartError {
    NotMainThread,
    Unsupported,
    LibraryLoad(PathBuf),
    NsApp(&'static str),
    Initialize,
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::NotMainThread => write!(f, "Chromium must start on the main thread"),
            StartError::Unsupported => write!(f, "Chromium is not supported on this platform yet"),
            StartError::LibraryLoad(p) => write!(f, "could not load Chromium from {}", p.display()),
            StartError::NsApp(e) => {
                write!(f, "could not prepare the application for Chromium: {e}")
            }
            StartError::Initialize => write!(f, "Chromium failed to initialize"),
        }
    }
}

impl std::error::Error for StartError {}

#[derive(Clone)]
struct AppConfig {
    framework_dir: String,
    mock_keychain: bool,
}

wrap_app! {
    struct MonocodeApp {
        config: AppConfig,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefString>,
            command_line: Option<&mut cef::CommandLine>,
        ) {
            let Some(command_line) = command_line else { return };
            // The app creates every browser itself.
            command_line.append_switch(Some(&"no-startup-window".into()));
            command_line.append_switch(Some(&"noerrdialogs".into()));
            command_line.append_switch(Some(&"hide-crash-restore-bubble".into()));
            if self.config.mock_keychain {
                command_line.append_switch(Some(&"use-mock-keychain".into()));
            }
        }

        fn browser_process_handler(&self) -> Option<cef::BrowserProcessHandler> {
            Some(MonocodeBrowserProcessHandler::new(self.config.clone()))
        }
    }
}

wrap_browser_process_handler! {
    struct MonocodeBrowserProcessHandler {
        config: AppConfig,
    }

    impl BrowserProcessHandler {
        fn on_before_child_process_launch(&self, command_line: Option<&mut cef::CommandLine>) {
            if let Some(command_line) = command_line {
                command_line.append_switch_with_value(
                    Some(&FRAMEWORK_SWITCH.into()),
                    Some(&self.config.framework_dir.as_str().into()),
                );
            }
        }

        fn on_schedule_message_pump_work(&self, delay_ms: i64) {
            crate::pump_schedule(delay_ms);
        }
    }
}

thread_local! {
    static STARTED: Cell<bool> = const { Cell::new(false) };
    /// [`shutdown`] ran, once for the process.
    static SHUTTING_DOWN: Cell<bool> = const { Cell::new(false) };
    /// Kept alive for as long as CEF runs.
    static APP: RefCell<Option<cef::App>> = const { RefCell::new(None) };
}

/// Whether [`start`] has succeeded in this process.
pub fn is_started() -> bool {
    STARTED.with(Cell::get)
}

/// Loads the runtime and initializes CEF. Idempotent; main thread only, after
/// the host created `NSApp`.
pub fn start(config: &RuntimeConfig) -> Result<(), StartError> {
    if is_started() {
        return Ok(());
    }
    platform_start(config)?;
    STARTED.with(|s| s.set(true));
    Ok(())
}

/// How long [`shutdown`] waits for the browsers to close.
pub const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Closes every browser and shuts CEF down, so that its helper processes
/// end with the app. For the app's quit, on the main thread, outside CEF's
/// callbacks. Runs CEF's message loop until each browser closed, for at
/// most `timeout`; if one is still open then, CEF stays up and ends with the
/// process. Does nothing when CEF never started, or the second time.
/// Whether CEF shut down.
pub fn shutdown(timeout: Duration) -> bool {
    if !is_started() || SHUTTING_DOWN.with(|s| s.replace(true)) {
        return false;
    }
    let deadline = Instant::now() + timeout;
    crate::popup::forget_all();
    crate::view::close_all();
    #[cfg(target_os = "macos")]
    let closed = crate::mac::pump::run_until(|| crate::view::live() == 0, deadline);
    #[cfg(not(target_os = "macos"))]
    let closed = {
        let _ = deadline;
        crate::view::live() == 0
    };
    if !closed {
        log::warn!(
            "browser: {} pages did not close in {timeout:?}; Chromium ends with the app",
            crate::view::live()
        );
        return false;
    }
    crate::download::forget_all();
    crate::profile::forget_all();
    #[cfg(target_os = "macos")]
    crate::mac::pump::stop();
    STARTED.with(|s| s.set(false));
    cef::shutdown();
    APP.with(|app| app.borrow_mut().take());
    true
}

#[cfg(target_os = "macos")]
fn load_library(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    cef::load_library(Some(unsafe { &*name.as_ptr().cast() })) == 1
}

#[cfg(target_os = "macos")]
fn platform_start(config: &RuntimeConfig) -> Result<(), StartError> {
    if objc2::MainThreadMarker::new().is_none() {
        return Err(StartError::NotMainThread);
    }
    let library = config.library_path();
    if !load_library(&library) {
        return Err(StartError::LibraryLoad(library));
    }
    // Pin the API version before any other CEF call.
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    crate::mac::nsapp::adopt_cef_app_protocol().map_err(StartError::NsApp)?;

    let framework_dir = &config.framework_dir;
    let profile = config.default_profile();
    let _ = std::fs::create_dir_all(&profile);
    let path = |p: &std::path::Path| CefString::from(p.to_string_lossy().as_ref());
    let settings = cef::Settings {
        no_sandbox: 0,
        browser_subprocess_path: path(&config.helper),
        framework_dir_path: path(framework_dir),
        // The host owns the run loop; CEF is driven by crate::mac::pump.
        multi_threaded_message_loop: 0,
        external_message_pump: 1,
        root_cache_path: path(&config.cache_root),
        cache_path: path(&profile),
        persist_session_cookies: 1,
        locale: config.locale.as_str().into(),
        log_file: config.log_file.as_deref().map(path).unwrap_or_default(),
        log_severity: if config.log_file.is_some() {
            cef::LogSeverity::WARNING
        } else {
            cef::LogSeverity::DISABLE
        },
        ..Default::default()
    };

    let mut app = MonocodeApp::new(AppConfig {
        framework_dir: framework_dir.to_string_lossy().into_owned(),
        mock_keychain: config.mock_keychain,
    });
    let args = cef::args::Args::new();
    if cef::initialize(
        Some(args.as_main_args()),
        Some(&settings),
        Some(&mut app),
        std::ptr::null_mut(),
    ) != 1
    {
        return Err(StartError::Initialize);
    }
    crate::mac::pump::install();
    crate::profile::set_root(config.profiles());
    APP.with(|slot| *slot.borrow_mut() = Some(app));
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn platform_start(_config: &RuntimeConfig) -> Result<(), StartError> {
    Err(StartError::Unsupported)
}
