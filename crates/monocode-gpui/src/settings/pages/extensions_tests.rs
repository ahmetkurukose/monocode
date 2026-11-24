use crate::settings::{Preferences, Section, SettingsContext, SettingsView};
use gpui::{
    AppContext, Bounds, TestAppContext, VisualTestContext, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use serde_json::json;

const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// A data folder with one installed extension (as the registry writes it).
fn data_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("Browser Extensions");
    let ext = root.join(ID).join("1.2.0");
    std::fs::create_dir_all(&ext).unwrap();
    std::fs::write(
        ext.join("manifest.json"),
        json!({"name": "Test Pass", "description": "Fills passwords.", "version": "1.2.0",
               "manifest_version": 3, "permissions": ["storage", "tabs", "identity"],
               "host_permissions": ["https://*/*"], "action": {"default_title": "Test Pass"}})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        root.join("extensions.json"),
        json!({"extensions": [{"id": ID, "version": "1.2.0", "enabled": true, "from_store": true,
               "granted_permissions": ["storage", "tabs", "identity"], "granted_hosts": ["https://*/*"]}]})
        .to_string(),
    )
    .unwrap();
    dir
}

fn window(dir: &std::path::Path, cx: &mut TestAppContext) -> WindowHandle<SettingsView> {
    let dir = dir.to_path_buf();
    cx.update(|cx| crate::browser::extensions::init(Some(dir), cx));
    let window = cx.update(|cx| {
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    point(px(0.), px(0.)),
                    size(px(1200.), px(3000.)),
                ))),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    SettingsView::new(
                        Section::Browser,
                        Preferences::default(),
                        SettingsContext::default(),
                        cx,
                    )
                })
            },
        )
        .unwrap()
    });
    cx.run_until_parked();
    window
}

fn click(visual: &mut VisualTestContext, selector: &'static str) {
    let at = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not drawn"))
        .center();
    visual.simulate_click(at, Default::default());
    visual.run_until_parked();
}

fn installed(dir: &std::path::Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join("Browser Extensions/extensions.json")).unwrap())
        .unwrap()
}

#[gpui::test]
fn the_page_lists_extensions_and_turns_them_off_and_removes_them(cx: &mut TestAppContext) {
    let dir = data_dir();
    let window = window(dir.path(), cx);
    let visual = &mut VisualTestContext::from_window(window.into(), cx);
    let listings = crate::browser::extensions::listings();
    assert_eq!(listings.len(), 1);
    assert_eq!(listings[0].name, "Test Pass");
    assert_eq!(
        listings[0].warnings,
        vec![
            "Read and change all your data on all websites",
            "Read your browsing history"
        ]
    );
    assert_eq!(listings[0].unsupported, vec!["identity"]);
    // A password manager it is not: ours keeps running.
    assert!(!crate::browser::extensions::password_manager_enabled());

    click(visual, "settings-extension-toggle-0");
    assert_eq!(installed(dir.path())["extensions"][0]["enabled"], false);
    click(visual, "settings-extension-toggle-0");
    assert_eq!(installed(dir.path())["extensions"][0]["enabled"], true);

    // Removing asks twice.
    click(visual, "settings-extension-remove-0");
    assert_eq!(
        installed(dir.path())["extensions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    click(visual, "settings-extension-remove-0");
    visual
        .executor()
        .advance_clock(std::time::Duration::from_secs(1));
    visual.run_until_parked();
    assert_eq!(installed(dir.path())["extensions"], json!([]));
    assert!(!dir.path().join("Browser Extensions").join(ID).exists());
    assert!(crate::browser::extensions::listings().is_empty());
}

#[gpui::test]
fn the_toolbar_shows_one_button_per_extension_with_an_action(cx: &mut TestAppContext) {
    let dir = data_dir();
    let _window = window(dir.path(), cx);
    cx.update(|_| {
        let shared =
            crate::browser::extensions::toolbar::buttons(&monocode_cef::Profile::Default, 1);
        assert_eq!(shared.len(), 1);
        // Throwaway agent profiles run no extension (K22).
        let throwaway = crate::browser::extensions::toolbar::buttons(
            &monocode_cef::Profile::Ephemeral("agent-1".into()),
            1,
        );
        assert!(throwaway.is_empty());
    });
    crate::browser::extensions::set_enabled(ID, false);
    cx.run_until_parked();
    cx.update(|_| {
        assert!(
            crate::browser::extensions::toolbar::buttons(&monocode_cef::Profile::Default, 1)
                .is_empty()
        );
    });
}
