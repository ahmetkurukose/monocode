//! The last stretch of a run's output as plain text, so an agent can read
//! what a run printed without a terminal: escape codes are taken out, the
//! size is bounded, and a reader can ask for only what is new.
use super::ports::first_local_url;

/// About how much text is kept per run.
const LIMIT: usize = 256 * 1024;
/// How far past a cut a line end is looked for, to start on a whole line.
const LINE_SEARCH: usize = 4096;

/// A piece of a run's output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunOutput {
    pub text: String,
    /// Pass this as `from` next time to get only what came after.
    pub next: u64,
    /// Output before `text` was asked for and is no longer kept.
    pub truncated: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Escape {
    #[default]
    None,
    /// After ESC.
    Started,
    /// In `ESC [ ...` until its final byte.
    Control,
    /// In a string (`ESC ]` title and the like) until BEL or `ESC \`.
    Text,
    TextEnding,
    /// One more character follows (`ESC ( B`).
    One,
}

/// Terminal output to plain text, across chunks that may split a sequence.
#[derive(Default)]
struct Plain {
    escape: Escape,
    /// A carriage return is waiting to see whether a line feed follows.
    carriage: bool,
}

impl Plain {
    fn push(&mut self, raw: &str, out: &mut String) {
        for c in raw.chars() {
            self.escape = match self.escape {
                Escape::None => match c {
                    '\u{1b}' => Escape::Started,
                    '\r' => {
                        self.carriage = true;
                        Escape::None
                    }
                    '\n' => {
                        self.carriage = false;
                        out.push('\n');
                        Escape::None
                    }
                    c if c.is_control() && c != '\t' => Escape::None,
                    c => {
                        // A progress line redrawn in place reads as lines.
                        if std::mem::take(&mut self.carriage) {
                            out.push('\n');
                        }
                        out.push(c);
                        Escape::None
                    }
                },
                Escape::Started => match c {
                    '[' => Escape::Control,
                    ']' | 'P' | 'X' | '^' | '_' => Escape::Text,
                    '(' | ')' | '*' | '+' | '#' | '%' => Escape::One,
                    _ => Escape::None,
                },
                Escape::Control if ('\u{40}'..='\u{7e}').contains(&c) => Escape::None,
                Escape::Control => Escape::Control,
                Escape::Text => match c {
                    '\u{7}' => Escape::None,
                    '\u{1b}' => Escape::TextEnding,
                    _ => Escape::Text,
                },
                Escape::TextEnding | Escape::One => Escape::None,
            };
        }
    }
}

#[derive(Default)]
pub(crate) struct Tail {
    text: String,
    /// The length of everything ever added; `text` is its end.
    total: u64,
    plain: Plain,
    /// The unfinished last line, while an address is still looked for.
    line: String,
    url: Option<String>,
}

impl Tail {
    /// Adds raw output. True when it announced the run's first local address.
    pub(crate) fn push(&mut self, raw: &str) -> bool {
        let before = self.text.len();
        self.plain.push(raw, &mut self.text);
        self.total += (self.text.len() - before) as u64;
        let found = self.url.is_none() && self.scan(before);
        if self.text.len() > LIMIT + LIMIT / 4 {
            let mut cut = self.text.len() - LIMIT;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            let ahead = &self.text.as_bytes()[cut..(cut + LINE_SEARCH).min(self.text.len())];
            if let Some(end) = ahead.iter().position(|byte| *byte == b'\n') {
                cut += end + 1;
            }
            self.text.drain(..cut);
        }
        found
    }

    /// Looks for an address in the lines completed since `before`.
    fn scan(&mut self, before: usize) -> bool {
        self.line.push_str(&self.text[before..]);
        while let Some(end) = self.line.find('\n') {
            let line: String = self.line.drain(..=end).collect();
            if let Some(url) = first_local_url(&line) {
                self.url = Some(url);
                self.line = String::new();
                return true;
            }
        }
        // No server announces itself on a line this long.
        if self.line.len() > LINE_SEARCH {
            self.line.clear();
        }
        false
    }

    pub(crate) fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    pub(crate) fn read(&self, from: u64) -> RunOutput {
        let start = self.total - self.text.len() as u64;
        let mut index = (from.clamp(start, self.total) - start) as usize;
        while !self.text.is_char_boundary(index) {
            index += 1;
        }
        RunOutput {
            text: self.text[index..].to_owned(),
            next: self.total,
            truncated: from < start,
        }
    }
}

/// Bytes from a pipe to text, holding back a character split across reads.
#[derive(Default)]
pub(crate) struct Decoder {
    carry: Vec<u8>,
}

impl Decoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> String {
        self.carry.extend_from_slice(bytes);
        let keep = match std::str::from_utf8(&self.carry) {
            Ok(_) => self.carry.len(),
            // Cut short at the end: the rest of the character comes next.
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(_) => self.carry.len(),
        };
        let text = String::from_utf8_lossy(&self.carry[..keep]).into_owned();
        self.carry.drain(..keep);
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(chunks: &[&str]) -> String {
        let mut tail = Tail::default();
        for chunk in chunks {
            tail.push(chunk);
        }
        tail.read(0).text
    }

    #[test]
    fn escape_codes_and_carriage_returns_are_taken_out() {
        let cases: [(&[&str], &str); 9] = [
            (&["plain\n"], "plain\n"),
            (&["\u{1b}[1;32mPASSED\u{1b}[0m ok\r\n"], "PASSED ok\n"),
            // A sequence split between two chunks.
            (&["a\u{1b}", "[3", "1mb\u{1b}[", "0mc"], "abc"),
            (&["\u{1b}]0;window title\u{7}text"], "text"),
            (
                &["\u{1b}]8;;http://x\u{1b}\\link\u{1b}]8;;\u{1b}\\"],
                "link",
            ),
            (&["\u{1b}(Bx\u{1b}=y"], "xy"),
            (&["10%\r50%\r100%\r\n", "done"], "10%\n50%\n100%\ndone"),
            (&["a\r", "\nb\r\r\nc"], "a\nb\nc"),
            (&["tab\there\u{7}\u{8}"], "tab\there"),
        ];
        for (chunks, expected) in cases {
            assert_eq!(plain(chunks), expected, "{chunks:?}");
        }
    }

    #[test]
    fn a_reader_gets_only_what_is_new() {
        let mut tail = Tail::default();
        tail.push("one\n");
        let first = tail.read(0);
        assert_eq!(
            (first.text.as_str(), first.next, first.truncated),
            ("one\n", 4, false)
        );
        assert_eq!(tail.read(first.next).text, "");
        tail.push("two\n");
        let second = tail.read(first.next);
        assert_eq!((second.text.as_str(), second.next), ("two\n", 8));
        // Past the end, or inside a character: nothing breaks.
        assert_eq!(tail.read(999).text, "");
        tail.push("é");
        assert_eq!(tail.read(9).text, "");
        assert_eq!(tail.read(8).text, "é");
    }

    #[test]
    fn the_tail_is_bounded_and_starts_on_a_line() {
        let mut tail = Tail::default();
        let line = "0123456789abcdef0123456789abcdé\n";
        let lines = (LIMIT * 2) / line.len();
        for _ in 0..lines {
            tail.push(line);
        }
        let all = tail.read(0);
        assert!(all.truncated);
        assert!(all.text.len() <= LIMIT + LIMIT / 4);
        assert!(all.text.len() >= LIMIT / 2);
        assert!(all.text.starts_with("0123"));
        assert!(all.text.ends_with("é\n"));
        assert_eq!(all.next, (lines * line.len()) as u64);
        // An offset from before the cut gives what is left, and says so.
        let late = tail.read(all.next - 2 * line.len() as u64);
        assert_eq!(late.text, format!("{line}{line}"));
        assert!(!late.truncated);
    }

    #[test]
    fn the_first_address_is_found_on_a_whole_line() {
        let mut tail = Tail::default();
        assert!(!tail.push("\u{1b}[32m➜\u{1b}[0m Local: http://local"));
        assert_eq!(tail.url(), None);
        assert!(!tail.push("host:\u{1b}[1m51"));
        assert!(tail.push("73\u{1b}[22m/\r\n"));
        assert_eq!(tail.url(), Some("http://localhost:5173/"));
        // Only the first is announced.
        assert!(!tail.push("also http://localhost:9999/\n"));
        assert_eq!(tail.url(), Some("http://localhost:5173/"));
    }

    #[test]
    fn split_characters_are_put_back_together() {
        let mut decoder = Decoder::default();
        let bytes = "aé€".as_bytes();
        let mut text = String::new();
        for byte in bytes {
            text.push_str(&decoder.push(&[*byte]));
        }
        assert_eq!(text, "aé€");
        assert_eq!(decoder.push(b"ok \xff bad"), "ok \u{fffd} bad");
    }
}
