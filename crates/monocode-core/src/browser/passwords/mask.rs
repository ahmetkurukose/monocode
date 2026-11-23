//! Agents may fill a saved login but never read it (context.md K57): every
//! answer an agent reads from a page (snapshots, eval results, console and
//! network logs, action effects, grabs) has the page's saved passwords
//! replaced with [`MASK`], also as they appear JSON-escaped or URL-encoded.
//! Best effort: a page that transforms a password (hashes, base64) hides it
//! from this too.

use crate::browser::{Content, ToolOutput};

/// What a password reads as.
pub const MASK: &str = "••••";

/// Passwords shorter than this are not masked in running text: they would
/// hide ordinary words.
pub const MIN_SECRET: usize = 4;

/// The forms `secret` may take in a page's text: as is, JSON-escaped,
/// URL-encoded (form and URI style). Longest first.
fn variants(secret: &str) -> Vec<String> {
    let mut out = vec![secret.to_owned()];
    if let Ok(json) = serde_json::to_string(secret) {
        out.push(json[1..json.len() - 1].to_owned());
    }
    let form: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
    out.push(form.clone());
    out.push(form.replace('+', "%20"));
    out.sort_by_key(|v| std::cmp::Reverse(v.len()));
    out.dedup();
    out
}

/// `text` with every form of every secret replaced by [`MASK`].
pub fn mask_text(text: &str, secrets: &[String]) -> String {
    let mut forms: Vec<String> = secrets
        .iter()
        .filter(|s| s.chars().count() >= MIN_SECRET)
        .flat_map(|s| variants(s))
        .collect();
    if forms.is_empty() {
        return text.to_owned();
    }
    forms.sort_by_key(|v| std::cmp::Reverse(v.len()));
    forms.dedup();
    let mut out = text.to_owned();
    for form in &forms {
        if out.contains(form.as_str()) {
            out = out.replace(form.as_str(), MASK);
        }
    }
    out
}

/// A tool's answer with [`mask_text`] applied to its text.
pub fn mask_output(output: ToolOutput, secrets: &[String]) -> ToolOutput {
    if secrets.is_empty() {
        return output;
    }
    ToolOutput {
        content: output
            .content
            .into_iter()
            .map(|c| match c {
                Content::Text(text) => Content::Text(mask_text(&text, secrets)),
                image => image,
            })
            .collect(),
        is_error: output.is_error,
    }
}

/// Whether an accessibility value is a password field's: Chromium shows
/// those as a row of bullets.
pub fn is_masked_value(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|c| matches!(c, '•' | '●' | '*'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secrets() -> Vec<String> {
        vec!["p@ss w\"rd&1".into(), "abc".into()]
    }

    #[test]
    fn a_saved_password_is_masked_in_every_form_it_takes() {
        let s = secrets();
        assert_eq!(mask_text("pw: p@ss w\"rd&1!", &s), "pw: ••••!");
        // JSON-escaped (eval results, console arguments).
        assert_eq!(
            mask_text(r#"{"pw":"p@ss w\"rd&1"}"#, &s),
            r#"{"pw":"••••"}"#
        );
        // URL-encoded (network log URLs, form bodies).
        assert_eq!(
            mask_text("POST /login?pw=p%40ss+w%22rd%261", &s),
            "POST /login?pw=••••"
        );
        assert_eq!(mask_text("?pw=p%40ss%20w%22rd%261", &s), "?pw=••••");
        // Too short to mask in running text.
        assert_eq!(mask_text("abc def", &s), "abc def");
        assert_eq!(mask_text("nothing here", &[]), "nothing here");
    }

    #[test]
    fn tool_answers_are_masked_text_by_text() {
        let output = ToolOutput {
            content: vec![
                Content::Text("value p@ss w\"rd&1".into()),
                Content::Image {
                    mime: "image/png".into(),
                    data: "p@ss w\"rd&1".into(),
                },
            ],
            is_error: true,
        };
        let masked = mask_output(output, &secrets());
        assert_eq!(masked.content[0], Content::Text("value ••••".into()));
        assert!(matches!(masked.content[1], Content::Image { .. }));
        assert!(masked.is_error);
    }

    #[test]
    fn chromium_bullets_are_a_password_fields_value() {
        assert!(is_masked_value("•••••••"));
        assert!(!is_masked_value("hunter2"));
        assert!(!is_masked_value(""));
    }
}
