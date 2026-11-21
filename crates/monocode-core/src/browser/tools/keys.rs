//! Key names to CDP key events on a US layout (after Orca's
//! `cdp-keyboard-us-layout.ts`): `Enter`, `a`, `Shift+ArrowDown`, `Meta+A`.

pub const ALT: i64 = 1;
pub const CTRL: i64 = 2;
pub const META: i64 = 4;
pub const SHIFT: i64 = 8;

/// One key press for `Input.dispatchKeyEvent`.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyPress {
    pub key: String,
    pub code: String,
    pub key_code: i64,
    pub modifiers: i64,
    /// 1 for the (left) modifier keys themselves.
    pub location: i64,
    /// The character it types; `None` for a raw key down.
    pub text: Option<String>,
    /// Editing commands Chromium runs for it (macOS shortcuts such as
    /// `Meta+A` do nothing without them).
    pub commands: Vec<&'static str>,
}

fn modifier_bit(name: &str) -> Option<i64> {
    Some(match name {
        "alt" | "option" => ALT,
        "ctrl" | "control" => CTRL,
        "cmd" | "command" | "meta" | "super" | "win" => META,
        "shift" => SHIFT,
        _ => return None,
    })
}

/// name → (windowsVirtualKeyCode, key, code, text)
fn named_key(name: &str) -> Option<(i64, &'static str, &'static str, Option<&'static str>)> {
    Some(match name {
        "enter" | "return" => (13, "Enter", "Enter", Some("\r")),
        "tab" => (9, "Tab", "Tab", None),
        "backspace" => (8, "Backspace", "Backspace", None),
        "delete" | "del" => (46, "Delete", "Delete", None),
        "escape" | "esc" => (27, "Escape", "Escape", None),
        "space" | "spacebar" => (32, " ", "Space", Some(" ")),
        "arrowup" | "up" => (38, "ArrowUp", "ArrowUp", None),
        "arrowdown" | "down" => (40, "ArrowDown", "ArrowDown", None),
        "arrowleft" | "left" => (37, "ArrowLeft", "ArrowLeft", None),
        "arrowright" | "right" => (39, "ArrowRight", "ArrowRight", None),
        "home" => (36, "Home", "Home", None),
        "end" => (35, "End", "End", None),
        "pageup" | "pgup" => (33, "PageUp", "PageUp", None),
        "pagedown" | "pgdn" | "pgdown" => (34, "PageDown", "PageDown", None),
        "insert" | "ins" => (45, "Insert", "Insert", None),
        "contextmenu" | "apps" => (93, "ContextMenu", "ContextMenu", None),
        "capslock" => (20, "CapsLock", "CapsLock", None),
        "shift" => (16, "Shift", "ShiftLeft", None),
        "control" | "ctrl" => (17, "Control", "ControlLeft", None),
        "alt" | "option" => (18, "Alt", "AltLeft", None),
        "meta" | "cmd" | "command" => (91, "Meta", "MetaLeft", None),
        _ => return None,
    })
}

/// A shifted US character and the key under it.
fn unshifted(ch: char) -> Option<char> {
    Some(match ch {
        '~' => '`',
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        ')' => '0',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        _ => return None,
    })
}

fn shifted(ch: char) -> Option<char> {
    "~!@#$%^&*()_+{}|:\"<>?"
        .chars()
        .find(|&s| unshifted(s) == Some(ch))
}

fn punctuation(ch: char) -> Option<(i64, &'static str)> {
    Some(match ch {
        ' ' => (32, "Space"),
        ';' => (186, "Semicolon"),
        '=' => (187, "Equal"),
        ',' => (188, "Comma"),
        '-' => (189, "Minus"),
        '.' => (190, "Period"),
        '/' => (191, "Slash"),
        '`' => (192, "Backquote"),
        '[' => (219, "BracketLeft"),
        '\\' => (220, "Backslash"),
        ']' => (221, "BracketRight"),
        '\'' => (222, "Quote"),
        _ => return None,
    })
}

/// (keyCode, code, needs shift) of the key that types `ch`.
fn us_key(ch: char) -> Option<(i64, String, bool)> {
    if ch.is_ascii_lowercase() {
        let upper = ch.to_ascii_uppercase();
        return Some((upper as i64, format!("Key{upper}"), false));
    }
    if ch.is_ascii_uppercase() {
        return Some((ch as i64, format!("Key{ch}"), true));
    }
    if ch.is_ascii_digit() {
        return Some((ch as i64, format!("Digit{ch}"), false));
    }
    if let Some(base) = unshifted(ch) {
        let (code, name, _) = us_key(base)?;
        return Some((code, name, true));
    }
    punctuation(ch).map(|(code, name)| (code, name.to_owned(), false))
}

/// Parses `Meta+Shift+z`, `Enter`, `F5`, `a`; `None` for an unknown key.
pub fn parse(raw: &str) -> Option<KeyPress> {
    let mut rest = raw;
    let mut modifiers = 0;
    while rest.chars().count() > 1 {
        let Some(plus) = rest.find('+').filter(|&p| p > 0) else {
            break;
        };
        let Some(bit) = modifier_bit(&rest[..plus].to_ascii_lowercase()) else {
            break;
        };
        modifiers |= bit;
        rest = &rest[plus + 1..];
    }
    if rest.is_empty() {
        return None;
    }
    let mut chars = rest.chars();
    let single = match (chars.next(), chars.next()) {
        (Some(ch), None) => Some(ch),
        _ => None,
    };
    let mut location = 0;
    let (key_code, mut key, code, mut text) = if let Some(ch) = single {
        let (key_code, code, shift) = us_key(ch)?;
        let mut key = ch.to_string();
        // Ctrl+A means the A key without Shift; shifted punctuation keeps it.
        if ch.is_ascii_uppercase() && modifiers & !SHIFT != 0 {
            key = ch.to_ascii_lowercase().to_string();
        } else if shift {
            modifiers |= SHIFT;
        }
        let text = Some(key.clone());
        (key_code, key, code, text)
    } else if let Some((key_code, key, code, text)) = named_key(&rest.to_ascii_lowercase()) {
        if let Some(bit) = modifier_bit(&rest.to_ascii_lowercase()) {
            // Blink sets a modifier's own bit on its key down.
            modifiers |= bit;
            location = 1;
        }
        (
            key_code,
            key.to_owned(),
            code.to_owned(),
            text.map(str::to_owned),
        )
    } else {
        let n: i64 = rest
            .strip_prefix(['f', 'F'])
            .and_then(|n| n.parse().ok())
            .filter(|n| (1..=24).contains(n))?;
        (111 + n, format!("F{n}"), format!("F{n}"), None)
    };
    if modifiers & SHIFT != 0 {
        if let Some(t) = text.take() {
            let mut cs = t.chars();
            let up = match (cs.next(), cs.next()) {
                (Some(c), None) => shifted(c)
                    .map(String::from)
                    .unwrap_or_else(|| c.to_uppercase().to_string()),
                _ => t,
            };
            if single.is_some() {
                key = up.clone();
            }
            text = Some(up);
        }
    }
    // Ctrl, Alt and Meta shortcuts type nothing.
    if modifiers & !SHIFT != 0 {
        text = None;
    }
    let commands = mac_commands(modifiers, &key);
    Some(KeyPress {
        key,
        code,
        key_code,
        modifiers,
        location,
        text,
        commands,
    })
}

/// The editing commands macOS binds to Meta shortcuts; Chromium does not
/// run them for synthetic key events by itself.
fn mac_commands(modifiers: i64, key: &str) -> Vec<&'static str> {
    let command = match (modifiers, key.to_ascii_lowercase().as_str()) {
        (META, "a") => "selectAll",
        (META, "c") => "copy",
        (META, "x") => "cut",
        (META, "v") => "paste",
        (META, "z") => "undo",
        (m, "z") if m == META | SHIFT => "redo",
        _ => return Vec::new(),
    };
    vec![command]
}

/// The key that types `ch`, or `None` when no key does (the text is then
/// inserted as is).
pub fn for_char(ch: char) -> Option<KeyPress> {
    match ch {
        '\n' => parse("Enter"),
        '\t' => parse("Tab"),
        '+' => parse("Shift+="),
        _ => parse(&ch.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_and_modifiers() {
        let enter = parse("Enter").unwrap();
        assert_eq!(
            (enter.key.as_str(), enter.code.as_str(), enter.key_code),
            ("Enter", "Enter", 13)
        );
        assert_eq!(enter.text.as_deref(), Some("\r"));

        let down = parse("Shift+ArrowDown").unwrap();
        assert_eq!(down.modifiers, SHIFT);
        assert_eq!(down.key_code, 40);
        assert_eq!(down.text, None);

        let all = parse("Meta+A").unwrap();
        assert_eq!(all.key, "a");
        assert_eq!(all.code, "KeyA");
        assert_eq!(all.modifiers, META);
        assert_eq!(all.text, None);
        assert_eq!(all.commands, vec!["selectAll"]);

        assert_eq!(parse("Meta+Shift+z").unwrap().commands, vec!["redo"]);
        assert_eq!(parse("Control+c").unwrap().commands, Vec::<&str>::new());
        assert_eq!(parse("F5").unwrap().key_code, 116);
        assert_eq!(parse("Shift").unwrap().location, 1);
        assert!(parse("Hyper+x").is_none());
        assert!(parse("NoSuchKey").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn printable_characters() {
        let a = parse("a").unwrap();
        assert_eq!(
            (a.key_code, a.text.as_deref(), a.modifiers),
            (65, Some("a"), 0)
        );
        let upper = parse("A").unwrap();
        assert_eq!((upper.text.as_deref(), upper.modifiers), (Some("A"), SHIFT));
        let bang = parse("!").unwrap();
        assert_eq!(
            (bang.key_code, bang.code.as_str(), bang.modifiers),
            (49, "Digit1", SHIFT)
        );
        assert_eq!(bang.text.as_deref(), Some("!"));
        let shift_one = parse("Shift+1").unwrap();
        assert_eq!(shift_one.text.as_deref(), Some("!"));
        assert_eq!(for_char('\n').unwrap().key, "Enter");
        assert_eq!(for_char('+').unwrap().text.as_deref(), Some("+"));
        assert!(for_char('é').is_none());
    }
}
