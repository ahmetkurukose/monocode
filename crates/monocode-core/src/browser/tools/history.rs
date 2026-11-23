//! `browser_history` (K65): the session's project's browsing history, and
//! never another project's. The host fills in the project from the scope.

use serde_json::Value;

use super::{Host, Reply, Scope, ToolOutput, invalid, quote};
use crate::browser::history::{Query, Transition};

const DEFAULT_LIMIT: u64 = 50;
const MAX_LIMIT: u64 = 500;

pub(super) fn history(host: &dyn Host, scope: &Scope, args: &Value, now_ms: i64) -> Reply {
    let text = args
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    let limit = match args.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(v) => v
            .as_u64()
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid("limit must be a positive integer"))?
            .min(MAX_LIMIT),
    };
    let since = match args.get("since") {
        None | Some(Value::Null) => None,
        Some(v) => Some(since(v, now_ms).ok_or_else(|| {
            invalid("since must be a duration back from now (30m, 2h, 7d), a date (2026-11-20), an RFC 3339 time, or Unix seconds")
        })?),
    };
    let query = Query {
        text: text.clone(),
        project: None,
        since,
        until: None,
        limit: limit as usize,
    };
    let visits = host
        .history(scope, &query)
        .map_err(|e| ToolOutput::error("browser_history_unavailable", e))?;
    if visits.is_empty() {
        return Ok(ToolOutput::text(if text.is_empty() {
            "No browsing history in this project for that time.".to_owned()
        } else {
            format!(
                "No page in this project's history matches {}.",
                quote(&text, 100)
            )
        }));
    }
    let offset = chrono::Local::now().format("%:z");
    let mut out = format!(
        "This project's browsing history, newest first ({} visit{}; local time, UTC{offset}):\n",
        visits.len(),
        if visits.len() == 1 { "" } else { "s" }
    );
    for visit in &visits {
        let at = chrono::DateTime::from_timestamp_millis(visit.visited_at)
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
            .unwrap_or_default();
        let mut notes = Vec::new();
        if visit.transition != Transition::Link {
            notes.push(visit.transition.as_str().replace('_', "/"));
        }
        if visit.visit_count > 1 {
            notes.push(format!("{} visits", visit.visit_count));
        }
        out.push_str(&format!(
            "{at}  {} {}{}\n",
            quote(&visit.title, 100),
            visit.url,
            if notes.is_empty() {
                String::new()
            } else {
                format!("  ({})", notes.join(", "))
            }
        ));
    }
    if visits.len() as u64 == limit {
        out.push_str("(There may be more: give a larger limit, a query or since.)\n");
    }
    Ok(ToolOutput::text(out.trim_end()))
}

/// `since` as milliseconds: `30m`/`2h`/`7d`/`1w` back from now, a date, an
/// RFC 3339 time, or Unix seconds (or milliseconds).
fn since(value: &Value, now_ms: i64) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        // Seconds until the year 2286; past that, milliseconds.
        return Some(if n < 10_000_000_000 { n * 1000 } else { n });
    }
    let text = value.as_str()?.trim();
    let duration = text.char_indices().last().and_then(|(i, c)| {
        Some((
            text[..i].trim().parse::<i64>().ok()?,
            c.to_ascii_lowercase(),
        ))
    });
    if let Some((n, unit)) = duration {
        let ms = match unit {
            's' => 1_000,
            'm' => 60_000,
            'h' => 3_600_000,
            'd' => 86_400_000,
            'w' => 7 * 86_400_000,
            _ => 0,
        };
        if ms > 0 && n >= 0 {
            return Some(now_ms - n.saturating_mul(ms));
        }
    }
    if let Ok(time) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(time.timestamp_millis());
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        let midnight = date.and_hms_opt(0, 0, 0)?;
        let local = midnight.and_local_timezone(chrono::Local).earliest()?;
        return Some(local.timestamp_millis());
    }
    text.parse::<i64>()
        .ok()
        .map(|n| if n < 10_000_000_000 { n * 1000 } else { n })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn since_reads_durations_dates_times_and_unix_seconds() {
        let now = 1_800_000_000_000;
        assert_eq!(since(&json!("30m"), now), Some(now - 1_800_000));
        assert_eq!(since(&json!("2h"), now), Some(now - 7_200_000));
        assert_eq!(since(&json!("7d"), now), Some(now - 7 * 86_400_000));
        assert_eq!(since(&json!("1W"), now), Some(now - 7 * 86_400_000));
        assert_eq!(since(&json!(1_700_000_000), now), Some(1_700_000_000_000));
        assert_eq!(
            since(&json!(1_700_000_000_123i64), now),
            Some(1_700_000_000_123)
        );
        assert_eq!(since(&json!("1700000000"), now), Some(1_700_000_000_000));
        assert_eq!(
            since(&json!("2026-11-20T10:00:00Z"), now),
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-11-20T10:00:00Z")
                    .unwrap()
                    .timestamp_millis()
            )
        );
        assert!(since(&json!("2026-11-20"), now).is_some());
        assert_eq!(since(&json!("yesterday"), now), None);
        assert_eq!(since(&json!(true), now), None);
    }
}
