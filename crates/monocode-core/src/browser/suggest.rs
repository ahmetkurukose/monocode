//! Search suggestions for Browser mode's Command Bar (docs/native/modes.md
//! §Command Bar), as SOCode's: what is typed goes to the search engine's
//! suggestion address (`socode_browser::omnibox::SearchEngine::suggest_url`)
//! on a fresh agent with no cookies, referrer, cache or profile, and the
//! answer's first few completions come back. Nothing else is sent.

use std::io::Read;
use std::time::Duration;

/// How long a request may take, and how much of the answer is read.
const TIMEOUT: Duration = Duration::from_secs(3);
const MAX_BYTES: u64 = 64 * 1024;
/// Completions at most.
pub const MAX_SUGGESTIONS: usize = 3;

/// The completions at `url` for `query`; empty on any failure.
pub fn fetch(url: &str, query: &str) -> Vec<String> {
    if url.is_empty() {
        return Vec::new();
    }
    let agent = ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .redirects(2)
        .build();
    let Ok(response) = agent.get(url).call() else {
        return Vec::new();
    };
    let mut body = String::new();
    if response
        .into_reader()
        .take(MAX_BYTES)
        .read_to_string(&mut body)
        .is_err()
    {
        return Vec::new();
    }
    parse(&body, query)
}

/// The completions in an OpenSearch suggestion answer (`["query",
/// ["completion", …]]`) for `query`: an answer for another query gives
/// none; repeats, the query itself, sums' answers and odd text are left out.
pub fn parse(body: &str, query: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    if value[0].as_str() != Some(query) {
        return Vec::new();
    }
    let Some(rows) = value[1].as_array() else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for text in rows.iter().filter_map(|row| row.as_str()) {
        let usable = text.len() <= 512
            && !text.trim().is_empty()
            && !text.contains(char::is_control)
            && text != query
            && !text.starts_with('=')
            && !out.iter().any(|seen| seen == text);
        if usable {
            out.push(text.to_string());
            if out.len() == MAX_SUGGESTIONS {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_bounded_deduplicated_and_bound_to_their_query() {
        assert_eq!(
            parse(
                r#"["rust",["rust","rust book","rust book",null,"rust docs","rust tutorial","rust more"]]"#,
                "rust"
            ),
            ["rust book", "rust docs", "rust tutorial"]
        );
        assert!(parse(r#"["old",["stale"]]"#, "new").is_empty());
        assert!(parse("invalid", "rust").is_empty());
        assert!(parse(r#"["2+2",["= 4"]]"#, "2+2").is_empty());
        assert!(fetch("", "rust").is_empty());
    }
}
