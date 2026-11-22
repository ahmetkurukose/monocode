use super::*;
use crate::browser::tools::fake::Script;
use futures_util::FutureExt;

#[test]
fn tokens_and_keys_are_redacted_and_ordinary_text_is_not() {
    let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
    let cases = [
        (
            "Authorization: Bearer abc.DEF-123_456~xyz",
            "Authorization: Bearer [redacted]",
        ),
        (jwt, "[redacted]"),
        ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "[redacted]"),
        ("key AKIAIOSFODNN7EXAMPLE here", "key [redacted] here"),
        ("sk_live_51HxYzABCdef1234", "[redacted]"),
        (
            "sha 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "sha [redacted]",
        ),
        (
            "blob Zm9vYmFyQmF6UXV4MTIzNDU2Nzg5MEFCQ0RFRkdISUo=",
            "blob [redacted]",
        ),
        (
            "/cb?code=abc123&state=ok&api_key=s3cr3t",
            "/cb?code=[redacted]&state=ok&api_key=[redacted]",
        ),
    ];
    for (input, want) in cases {
        assert_eq!(redact(input), want, "{input}");
    }
    for plain in [
        "Save changes",
        "https://example.com/docs/getting-started-with-the-new-dashboard-2024",
        "div.card > button.primary:nth-of-type(2)",
        "src/components/LoginForm.tsx:42:7",
    ] {
        assert_eq!(redact(plain), plain);
    }
}

#[test]
fn secret_looking_names_and_input_values_are_redacted() {
    for name in [
        "data-token",
        "data-api-key",
        "data-csrf",
        "data-session-id",
        "data-secret",
        "nonce",
        "data-key",
    ] {
        assert!(secret_name(name), "{name}");
    }
    for name in [
        "class",
        "href",
        "data-testid",
        "aria-label",
        "type",
        "data-index",
    ] {
        assert!(!secret_name(name), "{name}");
    }
    assert_eq!(redact_attribute("value", "hunter2"), "[redacted]");
    assert_eq!(redact_attribute("data-auth", "x"), "[redacted]");
    assert_eq!(redact_attribute("title", "Hello"), "Hello");
}

#[test]
fn html_loses_values_and_secrets_and_is_capped() {
    let html = r#"<form><input type="password" value="hunter2"><input type=hidden name=csrf value=tok_99><div data-api-key='k-1' class="row">Bearer abcdefghij1234</div></form>"#;
    let out = redact_html(html);
    for secret in ["hunter2", "tok_99", "k-1", "abcdefghij1234"] {
        assert!(!out.contains(secret), "{secret} in {out}");
    }
    assert!(out.contains(r#"class="row""#), "{out}");
    assert!(out.contains(r#"type="password""#), "{out}");

    let long = format!("<p>{}</p>", "é".repeat(5000));
    let out = redact_html(&long);
    assert!(out.len() <= MAX_HTML + 40, "{}", out.len());
    assert!(out.ends_with("<!-- cut at 4 KB -->"), "{out}");
}

#[test]
fn page_addresses_lose_query_fragment_and_credentials() {
    assert_eq!(
        page_url("https://user:pw@example.com/a/b?token=1#frag"),
        "https://example.com/a/b"
    );
    assert_eq!(page_url("about:blank"), "about:blank");
}

fn element() -> Element {
    Element {
        url: "https://shop.test/cart?session=abc".into(),
        title: "Cart".into(),
        tag: "button".into(),
        selector: "form#checkout > button.primary".into(),
        path: vec!["main".into(), "form".into()],
        role: String::new(),
        label: String::new(),
        text: "Pay now".into(),
        nearby: vec!["Total: $12".into(), " ".into()],
        attributes: vec![
            ("type".into(), "submit".into()),
            ("data-token".into(), "abc".into()),
        ],
        styles: vec![("display".into(), "flex".into())],
        html: "<button type=\"submit\">Pay now</button>".into(),
        rect: Rect {
            x: 10.,
            y: 20.,
            width: 120.,
            height: 32.,
        },
        viewport: Viewport {
            width: 800.,
            height: 600.,
            ..Viewport::default()
        },
        ax_role: "button".into(),
        ax_name: "Pay now".into(),
        components: vec!["Checkout".into(), "PayButton".into()],
        source: Some("webpack-internal:///./src/PayButton.tsx:12:5".into()),
        ..Element::default()
    }
}

#[test]
fn markdown_reads_like_a_report_of_the_element() {
    let grab = Grab {
        element: element().redacted(),
        image: None,
    };
    let md = grab.markdown();
    assert!(
        md.starts_with("Element picked in the browser on \"Cart\" <https://shop.test/cart>:"),
        "{md}"
    );
    for want in [
        "- Element: `<button>`",
        "- Selector: `form#checkout > button.primary`",
        "- Accessibility: button \"Pay now\"",
        "- React: `<Checkout> › <PayButton>`",
        "- Source: `src/PayButton.tsx:12:5`",
        "- Box: 120×32 at (10, 20) in a 800×600 viewport",
        "- Inside: `main > form`",
        "- Text: \"Pay now\"",
        "- Nearby text: \"Total: $12\"\n",
        "data-token=\"[redacted]\"",
        "```html\n<button type=\"submit\">Pay now</button>\n```",
    ] {
        assert!(md.contains(want), "{want:?} not in\n{md}");
    }
    assert!(!md.contains("session=abc"));
    assert_eq!(grab.label(), "button \"Pay now\"");
    assert_eq!(grab.file_stem(), "browser-button-pay-now");
}

#[test]
fn html_with_backticks_gets_a_longer_fence() {
    let mut e = element();
    e.html = "<pre>```js\nx\n```</pre>".into();
    let md = markdown(&e);
    assert!(md.contains("\n````html\n"), "{md}");
}

#[test]
fn long_texts_are_capped() {
    let mut e = element();
    e.text = "word ".repeat(200);
    e.nearby = (0..20).map(|i| format!("item {i}")).collect();
    let e = e.redacted();
    assert!(e.text.chars().count() <= MAX_TEXT + 1);
    assert_eq!(e.nearby.len(), MAX_NEARBY);
}

#[test]
fn the_crop_is_the_visible_part_in_page_coordinates() {
    let view = Viewport {
        width: 800.,
        height: 600.,
        scroll_x: 0.,
        scroll_y: 1000.,
        ratio: 2.,
    };
    let rect = Rect {
        x: -10.,
        y: 550.,
        width: 100.,
        height: 100.,
    };
    assert_eq!(
        clip(&rect, &view),
        Some(Rect {
            x: 0.,
            y: 1550.,
            width: 90.,
            height: 50.,
        })
    );
    let below = Rect { y: 700., ..rect };
    assert_eq!(clip(&below, &view), None);
    // At 150% the screenshot's clip counts the same box unzoomed.
    assert_eq!(
        zoomed(clip(&rect, &view).unwrap(), 1.5),
        Rect {
            x: 0.,
            y: 2325.,
            width: 135.,
            height: 75.,
        }
    );
    assert_eq!(zoomed(rect, 0.), rect);
}

fn png(len: usize) -> String {
    base64::engine::general_purpose::STANDARD.encode(vec![7u8; len])
}

/// A page whose picker reports `element()` and whose React and
/// accessibility answer.
fn page() -> (Rc<CdpClient>, Rc<Script>) {
    let (cdp, script) = Script::client();
    script.reply(
        "Page.getFrameTree",
        json!({"frameTree": {"frame": {"id": "F1"}}}),
    );
    script.reply("Page.createIsolatedWorld", json!({"executionContextId": 7}));
    script.on("Runtime.evaluate", |params, _| {
        let expression = params["expression"].as_str().unwrap_or("");
        Ok(if expression.ends_with(".picked") {
            json!({"result": {"type": "object", "objectId": "picked-1"}})
        } else if expression.contains("elementFromPoint") {
            json!({"result": {"type": "boolean", "value": true}})
        } else if expression.contains(".wait()") {
            json!({"result": {"type": "string", "value": "picked"}})
        } else {
            json!({"result": {"type": "boolean", "value": true}})
        })
    });
    script.on("Runtime.callFunctionOn", |params, _| {
        let function = params["functionDeclaration"].as_str().unwrap_or("");
        Ok(if function.contains("__reactFiber$") {
            json!({"result": {"value": {"components": ["App", "PayButton"], "source": "/src/PayButton.tsx:3"}}})
        } else {
            let raw = json!({
                "url": "https://shop.test/cart?session=abc",
                "title": "Cart",
                "tag": "button",
                "selector": "button.primary",
                "text": "Pay with ghp_abcdefghijklmnopqrstuvwxyz0123456789",
                "attributes": [["value", "hunter2"], ["class", "primary"]],
                "html": "<button value=\"hunter2\">Pay</button>",
                "rect": {"x": 10, "y": 20, "width": 100, "height": 30},
                "viewport": {"width": 800, "height": 600, "scroll_x": 0, "scroll_y": 40, "ratio": 2},
            });
            json!({"result": {"value": raw}})
        })
    });
    script.reply("DOM.describeNode", json!({"node": {"backendNodeId": 42}}));
    script.reply(
        "Accessibility.getPartialAXTree",
        json!({"nodes": [{"role": {"value": "button"}, "name": {"value": "Pay"}}]}),
    );
    script.reply("DOM.resolveNode", json!({"object": {"objectId": "main-1"}}));
    script.reply("Page.captureScreenshot", json!({"data": png(64)}));
    (cdp, script)
}

#[test]
fn a_right_click_grab_describes_the_element_under_the_pointer() {
    let (cdp, script) = page();
    let grab = grab_at(cdp, 15., 25.)
        .now_or_never()
        .expect("answered at once")
        .expect("grabbed");
    let e = &grab.element;
    assert_eq!((e.ax_role.as_str(), e.ax_name.as_str()), ("button", "Pay"));
    assert_eq!(e.components, ["App", "PayButton"]);
    assert_eq!(e.source.as_deref(), Some("/src/PayButton.tsx:3"));
    assert_eq!(e.url, "https://shop.test/cart");
    assert_eq!(e.text, "Pay with [redacted]");
    assert_eq!(e.attributes[0], ("value".into(), "[redacted]".into()));
    assert!(!e.html.contains("hunter2"));
    let image = grab.image.expect("a crop");
    assert_eq!((image.mime, image.bytes.len()), ("image/png", 64));

    // The picker's world is in the main frame; React is read in the page's.
    let world = &script.calls("Page.createIsolatedWorld")[0];
    assert_eq!(world["frameId"], "F1");
    let resolve = &script.calls("DOM.resolveNode")[0];
    assert_eq!(resolve["backendNodeId"], 42);
    assert!(resolve.get("executionContextId").is_none());
    let shot = &script.calls("Page.captureScreenshot")[0];
    assert_eq!(
        shot["clip"],
        json!({"x": 10.0, "y": 60.0, "width": 100.0, "height": 30.0, "scale": 1.0})
    );
}

#[test]
fn a_crop_over_two_megabytes_is_sent_as_a_smaller_jpeg() {
    let (cdp, script) = page();
    script.on("Page.captureScreenshot", |params, _| {
        let len = if params["format"] == "png" {
            MAX_IMAGE + 1
        } else {
            100
        };
        Ok(json!({"data": png(len)}))
    });
    let grab = grab_at(cdp, 1., 1.).now_or_never().unwrap().unwrap();
    let image = grab.image.unwrap();
    assert_eq!((image.mime, image.extension()), ("image/jpeg", "jpg"));
    assert_eq!(script.calls("Page.captureScreenshot").len(), 2);
}

#[test]
fn the_picker_arms_then_yields_the_pick_or_nothing_when_left() {
    let (cdp, script) = page();
    let picker = Picker::arm(cdp.clone()).now_or_never().unwrap().unwrap();
    let evaluated: Vec<String> = (script.calls("Runtime.evaluate").iter())
        .map(|p| p["expression"].as_str().unwrap_or("").to_owned())
        .collect();
    assert!(evaluated[0].contains("__monocodeGrab"), "the picker's code");
    assert_eq!(evaluated[1], "window.__monocodeGrab.arm()");
    assert!(
        (script.calls("Runtime.evaluate").iter()).all(|p| p["contextId"] == 7),
        "all in the isolated world"
    );
    let grab = picker.wait().now_or_never().unwrap().unwrap();
    assert_eq!(grab.unwrap().element.tag, "button");

    script.on("Runtime.evaluate", |_, _| {
        Ok(json!({"result": {"type": "string", "value": "cancelled"}}))
    });
    assert_eq!(picker.wait().now_or_never().unwrap(), Ok(None));
    // A navigation takes the world with it: also nothing.
    script.on("Runtime.evaluate", |_, _| {
        Err("Execution context was destroyed.".into())
    });
    assert_eq!(picker.wait().now_or_never().unwrap(), Ok(None));
    picker.cancel();
    let last = script.calls("Runtime.evaluate").pop().unwrap();
    assert!(last["expression"].as_str().unwrap().contains("cancel()"));
}
