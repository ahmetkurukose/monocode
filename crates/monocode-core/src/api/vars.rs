//! `{{variable}}` substitution. Layers stack: a later layer wins, so push the
//! collection's variables first and the active environment after them.
//! Postman's dynamic variables (`{{$guid}}`, `{{$timestamp}}`, ...) resolve
//! without a layer.

use super::model::KeyValue;
use std::ops::Range;

/// How deep a variable may refer to other variables before the chain is cut.
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, Default)]
pub struct Scope {
    layers: Vec<Vec<KeyValue>>,
}

/// The result of substituting one string.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Resolved {
    pub text: String,
    /// Names that had no value, left in the text as written.
    pub missing: Vec<String>,
    /// Names that are defined but empty, so they vanished from the text.
    pub empty: Vec<String>,
}

impl Scope {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a layer that wins over every layer added before it. Disabled
    /// rows are ignored.
    pub fn push(&mut self, rows: impl IntoIterator<Item = KeyValue>) -> &mut Self {
        self.layers
            .push(rows.into_iter().filter(|row| !row.disabled).collect());
        self
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.layers
            .iter()
            .rev()
            .flat_map(|layer| layer.iter().rev())
            .find(|row| row.key == name)
            .map(|row| row.value.as_str())
    }

    pub fn resolve(&self, input: &str) -> Resolved {
        let mut missing = Vec::new();
        let mut empty = Vec::new();
        let text = self.expand(input, 0, &mut missing, &mut empty);
        Resolved {
            text,
            missing,
            empty,
        }
    }

    fn expand(
        &self,
        input: &str,
        depth: usize,
        missing: &mut Vec<String>,
        empty: &mut Vec<String>,
    ) -> String {
        let mut out = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                out.push_str(&rest[start..]);
                return out;
            };
            let name = after[..end].trim();
            let literal = &rest[start..start + 2 + end + 2];
            match self.value_of(name, depth, missing, empty) {
                Some(value) => {
                    if value.is_empty() && !empty.iter().any(|known| known == name) {
                        empty.push(name.to_owned());
                    }
                    out.push_str(&value)
                }
                None => {
                    if !name.is_empty() && !missing.iter().any(|known| known == name) {
                        missing.push(name.to_owned());
                    }
                    out.push_str(literal);
                }
            }
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        out
    }

    fn value_of(
        &self,
        name: &str,
        depth: usize,
        missing: &mut Vec<String>,
        empty: &mut Vec<String>,
    ) -> Option<String> {
        if name.is_empty() {
            return None;
        }
        if let Some(value) = self.get(name) {
            return Some(if depth < MAX_DEPTH {
                self.expand(value, depth + 1, missing, empty)
            } else {
                value.to_owned()
            });
        }
        dynamic(name)
    }
}

/// Postman's built-in `{{$...}}` values.
fn dynamic(name: &str) -> Option<String> {
    Some(match name {
        "$guid" | "$randomUUID" => uuid::Uuid::new_v4().to_string(),
        "$timestamp" => chrono::Utc::now().timestamp().to_string(),
        "$isoTimestamp" => chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        "$randomInt" => rand::random_range(0..=1000).to_string(),
        "$randomAlphaNumeric" => {
            let n = rand::random_range(0..36u32);
            char::from_digit(n, 36)?.to_string()
        }
        _ => return None,
    })
}

/// Every `{{name}}` in `text`: its byte range (braces included) and name.
pub fn references(text: &str) -> Vec<(Range<usize>, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(start) = text[from..].find("{{") {
        let start = from + start;
        let Some(end) = text[start + 2..].find("}}") else {
            break;
        };
        let stop = start + 2 + end + 2;
        out.push((
            start..stop,
            text[start + 2..start + 2 + end].trim().to_owned(),
        ));
        from = stop;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(layers: &[&[(&str, &str)]]) -> Scope {
        let mut scope = Scope::new();
        for layer in layers {
            scope.push(layer.iter().map(|(k, v)| KeyValue::new(*k, *v)));
        }
        scope
    }

    #[test]
    fn later_layers_win_and_disabled_rows_are_ignored() {
        let mut s = scope(&[&[("host", "collection"), ("a", "1")], &[("host", "env")]]);
        s.push([KeyValue {
            key: "a".into(),
            value: "off".into(),
            disabled: true,
        }]);
        assert_eq!(s.resolve("{{host}}/{{a}}").text, "env/1");
    }

    #[test]
    fn variables_refer_to_other_variables() {
        let s = scope(&[&[("base", "https://{{host}}"), ("host", "x.test")]]);
        assert_eq!(s.resolve("{{base}}/v1").text, "https://x.test/v1");
    }

    #[test]
    fn a_cycle_is_cut_instead_of_looping() {
        let s = scope(&[&[("a", "{{b}}"), ("b", "{{a}}")]]);
        let out = s.resolve("{{a}}");
        assert!(out.text.len() < 200);
    }

    #[test]
    fn unknown_names_stay_as_written_and_are_reported_once() {
        let s = scope(&[&[("ok", "1")]]);
        let out = s.resolve("{{ ok }} {{nope}} {{nope}} {{ }} tail {{open");
        assert_eq!(out.text, "1 {{nope}} {{nope}} {{ }} tail {{open");
        assert_eq!(out.missing, vec!["nope"]);
    }

    #[test]
    fn dynamic_variables_resolve_without_a_layer() {
        let s = Scope::new();
        let out = s.resolve("{{$guid}}|{{$timestamp}}|{{$randomInt}}|{{$unknown}}");
        let parts: Vec<&str> = out.text.split('|').collect();
        assert_eq!(parts[0].len(), 36);
        assert!(parts[1].parse::<i64>().unwrap() > 1_700_000_000);
        assert!(parts[2].parse::<u32>().unwrap() <= 1000);
        assert_eq!(parts[3], "{{$unknown}}");
        assert_eq!(out.missing, vec!["$unknown"]);
    }

    #[test]
    fn defined_but_empty_variables_are_told_apart_from_missing_ones() {
        let s = scope(&[&[("url", ""), ("full", "{{url}}/x")]]);
        let out = s.resolve("{{url}}/flow {{full}} {{nope}}");
        assert_eq!(out.text, "/flow /x {{nope}}");
        assert_eq!(out.missing, vec!["nope"]);
        assert_eq!(out.empty, vec!["url"]);
    }

    #[test]
    fn references_report_ranges_for_highlighting() {
        let text = "a {{x}} b {{ y }}";
        let refs = references(text);
        assert_eq!(refs.len(), 2);
        assert_eq!(&text[refs[0].0.clone()], "{{x}}");
        assert_eq!(refs[1].1, "y");
    }
}
