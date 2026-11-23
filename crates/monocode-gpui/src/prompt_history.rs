//! The prompts the person sent, newest first, across every session. The
//! composer's history button lists them; picking one loads it back.
use gpui::{App, Global};
use std::path::PathBuf;

/// How many prompts are kept on disk.
const KEEP: usize = 50;
/// A menu row's label length before it trails off.
const LABEL_CHARS: usize = 72;

#[derive(Default)]
struct PromptHistory {
    entries: Vec<String>,
}
impl Global for PromptHistory {}

fn path() -> Option<PathBuf> {
    // Tests never read or write the person's own history.
    (!cfg!(test)).then(|| crate::data_dir().join("prompt-history.json"))
}

fn ensure(cx: &mut App) {
    if cx.has_global::<PromptHistory>() {
        return;
    }
    let entries = path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
        .unwrap_or_default();
    cx.set_global(PromptHistory { entries });
}

/// The remembered prompts, newest first.
pub(crate) fn entries(cx: &mut App) -> Vec<String> {
    ensure(cx);
    cx.global::<PromptHistory>().entries.clone()
}

/// Moves `prompt` to the front of the history and saves it.
pub(crate) fn remember(prompt: &str, cx: &mut App) {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return;
    }
    ensure(cx);
    let entries = {
        let history = cx.global_mut::<PromptHistory>();
        push(&mut history.entries, prompt);
        history.entries.clone()
    };
    if let Some(path) = path() {
        cx.background_executor()
            .spawn(async move {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Ok(json) = serde_json::to_vec(&entries) {
                    let _ = std::fs::write(path, json);
                }
            })
            .detach();
    }
}

fn push(entries: &mut Vec<String>, prompt: &str) {
    entries.retain(|old| old != prompt);
    entries.insert(0, prompt.to_owned());
    entries.truncate(KEEP);
}

/// One line of the prompt, cut with an ellipsis when it runs long.
pub(crate) fn label(prompt: &str) -> String {
    let line = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= LABEL_CHARS {
        return line;
    }
    let cut: String = line.chars().take(LABEL_CHARS).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_without_duplicates_and_capped() {
        let mut entries = Vec::new();
        for n in 0..60 {
            push(&mut entries, &format!("p{n}"));
        }
        push(&mut entries, "p30");
        assert_eq!(entries.len(), KEEP);
        assert_eq!(entries[0], "p30");
        assert_eq!(entries[1], "p59");
        assert_eq!(entries.iter().filter(|e| *e == "p30").count(), 1);
    }

    #[test]
    fn labels_are_one_line_and_trail_off_when_long() {
        assert_eq!(label("fix\n  the bug"), "fix the bug");
        let long = "word ".repeat(40);
        let shown = label(&long);
        assert!(shown.ends_with('…'));
        assert!(shown.chars().count() <= LABEL_CHARS + 1);
    }
}
