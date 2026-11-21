//! Chromium's child processes (renderer, GPU, network, utility).
//!
//! The framework path comes from the `--monocode-cef-framework=` switch the
//! browser process adds to every child's command line
//! (`monocode_cef::runtime::FRAMEWORK_SWITCH`).

#[cfg(target_os = "macos")]
fn main() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, path::PathBuf};

    let switch = format!("--{}=", monocode_cef::runtime::FRAMEWORK_SWITCH);
    let Some(framework) = std::env::args().find_map(|a| a.strip_prefix(&switch).map(PathBuf::from))
    else {
        eprintln!("monocode-cef-helper: missing {switch}");
        std::process::exit(1);
    };
    let args = cef::args::Args::new();

    // The sandbox library must be initialized before CEF itself is loaded.
    let sandbox = framework.join("Libraries/libcef_sandbox.dylib");
    let sandbox = CString::new(sandbox.as_os_str().as_bytes()).expect("path");
    let handle = unsafe { libc::dlopen(sandbox.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    let mut context = std::ptr::null_mut();
    if !handle.is_null() {
        type Init = unsafe extern "C" fn(i32, *mut *mut std::ffi::c_char) -> *mut std::ffi::c_void;
        let init = unsafe { libc::dlsym(handle, c"cef_sandbox_initialize".as_ptr()) };
        if !init.is_null() {
            let main_args = args.as_main_args();
            context = unsafe {
                std::mem::transmute::<*mut std::ffi::c_void, Init>(init)(
                    main_args.argc,
                    main_args.argv,
                )
            };
        }
    }
    if context.is_null() {
        eprintln!("monocode-cef-helper: could not initialize the sandbox");
        std::process::exit(1);
    }

    let library = framework.join("Chromium Embedded Framework");
    let library = CString::new(library.as_os_str().as_bytes()).expect("path");
    if cef::load_library(Some(unsafe { &*library.as_ptr().cast() })) != 1 {
        eprintln!(
            "monocode-cef-helper: could not load {}",
            framework.display()
        );
        std::process::exit(1);
    }
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let code = cef::execute_process(
        Some(args.as_main_args()),
        None::<&mut cef::App>,
        std::ptr::null_mut(),
    );
    std::process::exit(code);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    // Windows and Linux arrive in phase 6 (context.md §12).
    eprintln!("monocode-cef-helper: this platform is not supported yet");
    std::process::exit(1);
}
