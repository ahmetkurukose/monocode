//! Kanban notes: a board of tasks in four columns, kept as JSON in a note's
//! body. The columns, quick-add syntax and time formats follow Blitzit:
//! Backlog, This Week, Today and Done; "Write report 1h30m @today" adds a
//! 90-minute task to Today.
use serde::{Deserialize, Serialize};

/// The note type a kanban note carries in the note languages table.
pub const KANBAN: &str = "kanban";
pub const MAX_TASK_TITLE: usize = 500;
pub const MAX_TASK_NOTES: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Column {
    Backlog,
    ThisWeek,
    Today,
    Done,
}

impl Column {
    pub const ALL: [Column; 4] = [
        Column::Backlog,
        Column::ThisWeek,
        Column::Today,
        Column::Done,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Column::Backlog => "Backlog",
            Column::ThisWeek => "This Week",
            Column::Today => "Today",
            Column::Done => "Done",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Column::Backlog => "backlog",
            Column::ThisWeek => "this-week",
            Column::Today => "today",
            Column::Done => "done",
        }
    }

    pub fn index(self) -> usize {
        Column::ALL.iter().position(|c| *c == self).unwrap_or(0)
    }

    /// The column beside this one, `-1` left or `1` right.
    pub fn step(self, direction: i32) -> Option<Column> {
        let next = self.index() as i32 + direction;
        (0..Column::ALL.len() as i32)
            .contains(&next)
            .then(|| Column::ALL[next as usize])
    }

    /// The column an `@tag` names, in English or Turkish.
    pub fn from_tag(tag: &str) -> Option<Column> {
        match tag.to_lowercase().as_str() {
            "today" | "bugun" | "bugün" => Some(Column::Today),
            "week" | "this-week" | "thisweek" | "hafta" => Some(Column::ThisWeek),
            "backlog" => Some(Column::Backlog),
            "done" => Some(Column::Done),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub notes: String,
    pub column: Column,
    /// The estimate, in minutes; 0 is none.
    #[serde(default)]
    pub estimate_min: u32,
    /// The time taken so far, in minutes.
    #[serde(default)]
    pub taken_min: u32,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<i64>,
}

impl Task {
    pub fn done(&self) -> bool {
        self.column == Column::Done
    }

    /// Its first line of notes, for the card.
    pub fn notes_preview(&self) -> Option<&str> {
        self.notes
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
    }

    pub fn remaining_min(&self) -> u32 {
        self.estimate_min.saturating_sub(self.taken_min)
    }

    pub fn over_estimate(&self) -> bool {
        self.estimate_min > 0 && self.taken_min > self.estimate_min
    }
}

/// A board's tasks; each column lists its own in the order kept here.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Board {
    #[serde(default)]
    pub tasks: Vec<Task>,
}

/// Local-time boundaries progress and Done's count are measured from.
#[derive(Clone, Copy, Debug)]
pub struct Periods {
    pub day: i64,
    pub week: i64,
    pub month: i64,
}

impl Periods {
    pub fn now() -> Self {
        Self::at(chrono::Local::now())
    }

    pub fn at<Tz: chrono::TimeZone>(now: chrono::DateTime<Tz>) -> Self {
        use chrono::Datelike;
        let start = |date: chrono::NaiveDate| {
            date.and_hms_opt(0, 0, 0)
                .and_then(|time| time.and_local_timezone(now.timezone()).earliest())
                .map_or(0, |time| time.timestamp_millis())
        };
        let today = now.date_naive();
        let week = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
        let month = today.with_day(1).unwrap_or(today);
        Self {
            day: start(today),
            week: start(week),
            month: start(month),
        }
    }
}

/// Done of total, for a column's progress bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

impl Progress {
    pub fn fraction(self) -> f32 {
        if self.total == 0 {
            0.
        } else {
            self.done as f32 / self.total as f32
        }
    }
}

impl Board {
    /// The board a note's body holds; an empty or unreadable body is an empty board.
    pub fn parse(body: &str) -> Self {
        serde_json::from_str(body).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{\"tasks\":[]}".into())
    }

    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|task| task.id == id)
    }

    fn task_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.iter_mut().find(|task| task.id == id)
    }

    pub fn column(&self, column: Column) -> impl Iterator<Item = &Task> {
        self.tasks.iter().filter(move |task| task.column == column)
    }

    pub fn pending(&self) -> impl Iterator<Item = &Task> {
        // Today first, then This Week, then Backlog, as Blitzit counts them.
        [Column::Today, Column::ThisWeek, Column::Backlog]
            .into_iter()
            .flat_map(|column| self.column(column))
    }

    /// The estimate still left on a column's tasks, in minutes.
    pub fn remaining(&self, column: Column) -> u32 {
        self.column(column).map(Task::remaining_min).sum()
    }

    pub fn pending_remaining(&self) -> u32 {
        self.pending().map(Task::remaining_min).sum()
    }

    pub fn done_since(&self, since: i64) -> usize {
        self.column(Column::Done)
            .filter(|task| task.done_at.is_some_and(|at| at >= since))
            .count()
    }

    /// Today: what got done today of today's work. This Week: what got done
    /// this week of the week's work, Today's included. No bar elsewhere.
    pub fn progress(&self, column: Column, periods: Periods) -> Option<Progress> {
        match column {
            Column::Today => {
                let done = self.done_since(periods.day);
                Some(Progress {
                    done,
                    total: done + self.column(Column::Today).count(),
                })
            }
            Column::ThisWeek => {
                let done = self.done_since(periods.week);
                Some(Progress {
                    done,
                    total: done
                        + self.column(Column::ThisWeek).count()
                        + self.column(Column::Today).count(),
                })
            }
            _ => None,
        }
    }

    /// Adds a task at the end of its column and returns its id.
    pub fn add(&mut self, title: &str, estimate_min: u32, column: Column, now: i64) -> String {
        let id = format!("task-{now}-{:08x}", rand::random::<u32>());
        let title = title.trim();
        self.tasks.push(Task {
            id: id.clone(),
            title: if title.is_empty() {
                "Untitled task".into()
            } else {
                title.chars().take(MAX_TASK_TITLE).collect()
            },
            notes: String::new(),
            column,
            estimate_min,
            taken_min: 0,
            created_at: now,
            done_at: (column == Column::Done).then_some(now),
        });
        id
    }

    /// Adds what the quick-add field holds: `@tag` and duration override
    /// `column` and the separate estimate when they are given.
    pub fn quick_add(
        &mut self,
        input: &str,
        estimate: &str,
        column: Column,
        now: i64,
    ) -> Option<String> {
        let raw = input.trim();
        if raw.is_empty() {
            return None;
        }
        let parsed = parse_quick_add(raw);
        let typed = parse_hhmm(estimate);
        let title = if parsed.title.is_empty() {
            raw
        } else {
            &parsed.title
        };
        Some(self.add(
            title,
            if typed > 0 {
                typed
            } else {
                parsed.estimate_min
            },
            parsed.column.unwrap_or(column),
            now,
        ))
    }

    /// Puts a task in `column` at `index` among that column's other tasks;
    /// past the end it goes last. Entering or leaving Done stamps or clears
    /// its finish time.
    pub fn move_task(&mut self, id: &str, column: Column, index: usize, now: i64) {
        let Some(from) = self.tasks.iter().position(|task| task.id == id) else {
            return;
        };
        let mut task = self.tasks.remove(from);
        if task.column != column {
            task.done_at = (column == Column::Done).then_some(now);
            task.column = column;
        }
        let at = self
            .tasks
            .iter()
            .enumerate()
            .filter(|(_, other)| other.column == column)
            .nth(index)
            .map(|(at, _)| at)
            .or_else(|| {
                self.tasks
                    .iter()
                    .rposition(|other| other.column == column)
                    .map(|at| at + 1)
            })
            .unwrap_or(self.tasks.len());
        self.tasks.insert(at, task);
    }

    /// Moves a task to the end of the column beside its own.
    pub fn shift(&mut self, id: &str, direction: i32, now: i64) {
        let Some(column) = self.task(id).and_then(|task| task.column.step(direction)) else {
            return;
        };
        self.move_task(id, column, usize::MAX, now);
    }

    /// Done goes back to Today; anything else is done.
    pub fn toggle_done(&mut self, id: &str, now: i64) {
        let Some(task) = self.task(id) else { return };
        let column = if task.done() {
            Column::Today
        } else {
            Column::Done
        };
        self.move_task(id, column, usize::MAX, now);
    }

    /// A copy right after the task, with no time taken.
    pub fn duplicate(&mut self, id: &str, now: i64) -> Option<String> {
        let at = self.tasks.iter().position(|task| task.id == id)?;
        let mut copy = self.tasks[at].clone();
        copy.id = format!("task-{now}-{:08x}", rand::random::<u32>());
        copy.taken_min = 0;
        copy.created_at = now;
        let id = copy.id.clone();
        self.tasks.insert(at + 1, copy);
        Some(id)
    }

    pub fn remove(&mut self, id: &str) {
        self.tasks.retain(|task| task.id != id);
    }

    pub fn update(
        &mut self,
        id: &str,
        title: Option<&str>,
        notes: Option<&str>,
        estimate_min: Option<u32>,
        taken_min: Option<u32>,
    ) {
        let Some(task) = self.task_mut(id) else {
            return;
        };
        if let Some(title) = title.map(str::trim).filter(|title| !title.is_empty()) {
            task.title = title.chars().take(MAX_TASK_TITLE).collect();
        }
        if let Some(notes) = notes {
            task.notes = notes.chars().take(MAX_TASK_NOTES).collect();
        }
        if let Some(estimate) = estimate_min {
            task.estimate_min = estimate;
        }
        if let Some(taken) = taken_min {
            task.taken_min = taken;
        }
    }

    /// The list's one-line summary.
    pub fn summary(&self) -> String {
        let pending = self.pending().count();
        let done = self.column(Column::Done).count();
        match (pending, done) {
            (0, 0) => "No tasks yet".into(),
            (0, done) => format!("All clear · {done} done"),
            (pending, done) => {
                let est = self.pending_remaining();
                let mut text = format!(
                    "{pending} pending task{}",
                    if pending == 1 { "" } else { "s" }
                );
                if est > 0 {
                    text.push_str(&format!(", Est: {}", est_label(est, true)));
                }
                if done > 0 {
                    text.push_str(&format!(" · {done} done"));
                }
                text
            }
        }
    }

    /// The board as Markdown checklists, for a chat or another reader.
    pub fn to_markdown(&self, title: &str) -> String {
        let mut out = format!("# {}\n", title.trim());
        for column in Column::ALL {
            let tasks: Vec<_> = self.column(column).collect();
            if tasks.is_empty() {
                continue;
            }
            out.push_str(&format!("\n## {}\n\n", column.label()));
            for task in tasks {
                out.push_str(&format!(
                    "- [{}] {}",
                    if task.done() { "x" } else { " " },
                    task.title
                ));
                if task.estimate_min > 0 || task.taken_min > 0 {
                    out.push_str(&format!(
                        " (est {}, taken {})",
                        hhmm(task.estimate_min),
                        hhmm(task.taken_min)
                    ));
                }
                out.push('\n');
                for line in task.notes.lines().filter(|line| !line.trim().is_empty()) {
                    out.push_str(&format!("  > {}\n", line.trim()));
                }
            }
        }
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuickAdd {
    pub title: String,
    pub estimate_min: u32,
    pub column: Option<Column>,
}

/// "Write report 1h30m @today": durations (`45m`, `1.5h`, `1h30m`,
/// `2sa 15dk`, `1:30`) add up to the estimate and `@today`, `@week` or
/// `@backlog` picks the column; the rest is the title.
pub fn parse_quick_add(input: &str) -> QuickAdd {
    static TAG: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"(?:^|\s)@([\p{L}-]+)(?:\s|$)").unwrap());
    static DURATION: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)(?:^|\s)(?:(\d{1,2}):([0-5]\d)|(\d+(?:[.,]\d+)?)\s*(?:h|sa|saat)(?:\s*(\d+)\s*(?:m|dk|min))?|(\d+)\s*(?:m|dk|min))(?:\s|$)",
        )
        .unwrap()
    });
    let mut text = format!(" {input} ");
    let mut column = None;
    // Matches share their spaces, so replace until nothing more is found.
    loop {
        let Some(capture) = TAG
            .captures_iter(&text)
            .find(|capture| Column::from_tag(&capture[1]).is_some())
        else {
            break;
        };
        column = Column::from_tag(&capture[1]);
        let range = capture.get(0).unwrap().range();
        text.replace_range(range, "  ");
    }
    let mut minutes = 0_f64;
    while let Some(capture) = DURATION.captures(&text) {
        if let (Some(hours), Some(mins)) = (capture.get(1), capture.get(2)) {
            minutes += hours.as_str().parse::<f64>().unwrap_or(0.) * 60.
                + mins.as_str().parse::<f64>().unwrap_or(0.);
        } else if let Some(hours) = capture.get(3) {
            minutes += hours
                .as_str()
                .replace(',', ".")
                .parse::<f64>()
                .unwrap_or(0.)
                * 60.
                + capture
                    .get(4)
                    .map_or(0., |m| m.as_str().parse().unwrap_or(0.));
        } else if let Some(mins) = capture.get(5) {
            minutes += mins.as_str().parse::<f64>().unwrap_or(0.);
        }
        let range = capture.get(0).unwrap().range();
        text.replace_range(range, "  ");
    }
    QuickAdd {
        title: text.split_whitespace().collect::<Vec<_>>().join(" "),
        estimate_min: minutes.round() as u32,
        column,
    }
}

/// "00:40", "40", "130" (1:30), "1h30m" or "2sa 15dk", in minutes.
pub fn parse_hhmm(input: &str) -> u32 {
    let text = input.trim();
    if text.is_empty() {
        return 0;
    }
    if let Some((hours, mins)) = text.split_once(':') {
        if let (Ok(hours), Ok(mins)) = (hours.parse::<u32>(), mins.parse::<u32>()) {
            return hours * 60 + mins;
        }
    }
    if text.chars().all(|ch| ch.is_ascii_digit()) {
        let value: u32 = text.parse().unwrap_or(0);
        return if (3..=4).contains(&text.len()) {
            value / 100 * 60 + value % 100
        } else {
            value
        };
    }
    static PART: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(\d+(?:[.,]\d+)?)\s*(h|sa|m|dk)").unwrap()
    });
    let mut minutes = 0_f64;
    for capture in PART.captures_iter(text) {
        let value = capture[1].replace(',', ".").parse::<f64>().unwrap_or(0.);
        minutes += if matches!(capture[2].to_lowercase().as_str(), "h" | "sa") {
            value * 60.
        } else {
            value
        };
    }
    minutes.round() as u32
}

/// Typing into a time field keeps it to `##:##`.
pub fn mask_hhmm(input: &str) -> String {
    let digits: String = input.chars().filter(char::is_ascii_digit).take(4).collect();
    if digits.len() > 2 {
        format!(
            "{}:{}",
            &digits[..digits.len() - 2],
            &digits[digits.len() - 2..]
        )
    } else {
        digits
    }
}

/// 40 → "00:40", the card's time format.
pub fn hhmm(minutes: u32) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// 80 → "1hr 20min", or "1hr 20mins" with `plural`.
pub fn est_label(minutes: u32, plural: bool) -> String {
    let (hours, mins) = (minutes / 60, minutes % 60);
    let mut parts = Vec::new();
    if hours > 0 {
        parts.push(format!("{hours}hr"));
    }
    if mins > 0 {
        parts.push(format!(
            "{mins}min{}",
            if plural && mins > 1 { "s" } else { "" }
        ));
    }
    if parts.is_empty() {
        "0min".into()
    } else {
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn titles(board: &Board, column: Column) -> Vec<&str> {
        board
            .column(column)
            .map(|task| task.title.as_str())
            .collect()
    }

    #[test]
    fn quick_add_reads_durations_and_columns_out_of_the_title() {
        let parsed = parse_quick_add("Rapor yaz 1h30m @today");
        assert_eq!(parsed.title, "Rapor yaz");
        assert_eq!(parsed.estimate_min, 90);
        assert_eq!(parsed.column, Some(Column::Today));
        assert_eq!(parse_quick_add("Call 45m").estimate_min, 45);
        assert_eq!(
            parse_quick_add("Read 1.5h @hafta").column,
            Some(Column::ThisWeek)
        );
        assert_eq!(parse_quick_add("Read 1,5h").estimate_min, 90);
        assert_eq!(parse_quick_add("Gym 2sa 15dk").estimate_min, 135);
        assert_eq!(parse_quick_add("Sync 1:30").estimate_min, 90);
        let plain = parse_quick_add("Email @someone about 3 apples");
        assert_eq!(plain.title, "Email @someone about 3 apples");
        assert_eq!(plain.estimate_min, 0);
        assert_eq!(plain.column, None);
    }

    #[test]
    fn time_fields_take_hhmm_bare_minutes_and_units() {
        assert_eq!(parse_hhmm("00:40"), 40);
        assert_eq!(parse_hhmm("1:30"), 90);
        assert_eq!(parse_hhmm("40"), 40);
        assert_eq!(parse_hhmm("130"), 90);
        assert_eq!(parse_hhmm("1h30m"), 90);
        assert_eq!(parse_hhmm(""), 0);
        assert_eq!(mask_hhmm("0130"), "01:30");
        assert_eq!(mask_hhmm("4a5"), "45");
        assert_eq!(hhmm(90), "01:30");
        assert_eq!(est_label(80, true), "1hr 20mins");
        assert_eq!(est_label(61, true), "1hr 1min");
        assert_eq!(est_label(0, false), "0min");
    }

    #[test]
    fn moving_reorders_within_a_column_and_stamps_done() {
        let mut board = Board::default();
        let a = board.add("a", 0, Column::Today, 1);
        let b = board.add("b", 0, Column::Today, 1);
        let c = board.add("c", 0, Column::Backlog, 1);
        board.move_task(&b, Column::Today, 0, 2);
        assert_eq!(titles(&board, Column::Today), ["b", "a"]);
        board.move_task(&c, Column::Today, 1, 2);
        assert_eq!(titles(&board, Column::Today), ["b", "c", "a"]);
        board.move_task(&a, Column::Done, 0, 5);
        assert_eq!(board.task(&a).unwrap().done_at, Some(5));
        board.toggle_done(&a, 6);
        assert_eq!(board.task(&a).unwrap().column, Column::Today);
        assert_eq!(board.task(&a).unwrap().done_at, None);
        assert_eq!(titles(&board, Column::Today), ["b", "c", "a"]);
        board.shift(&b, -1, 7);
        assert_eq!(titles(&board, Column::ThisWeek), ["b"]);
        board.shift(&c, -1, 7);
        board.shift(&c, -1, 7);
        board.shift(&c, -1, 7);
        assert_eq!(board.task(&c).unwrap().column, Column::Backlog);
    }

    #[test]
    fn progress_counts_what_got_done_in_the_period() {
        let mut board = Board::default();
        let periods = Periods {
            day: 100,
            week: 50,
            month: 10,
        };
        let old = board.add("old", 0, Column::Today, 1);
        board.move_task(&old, Column::Done, 0, 60);
        let new = board.add("new", 0, Column::Today, 1);
        board.move_task(&new, Column::Done, 0, 120);
        board.add("left", 30, Column::Today, 1);
        board.add("week", 0, Column::ThisWeek, 1);
        assert_eq!(
            board.progress(Column::Today, periods),
            Some(Progress { done: 1, total: 2 })
        );
        assert_eq!(
            board.progress(Column::ThisWeek, periods),
            Some(Progress { done: 2, total: 4 })
        );
        assert_eq!(board.progress(Column::Backlog, periods), None);
        assert_eq!(board.done_since(periods.month), 2);
        assert_eq!(board.remaining(Column::Today), 30);
    }

    #[test]
    fn a_board_round_trips_through_json_and_reads_as_markdown() {
        let mut board = Board::default();
        let id = board.add("Write", 90, Column::Today, 1);
        board.update(&id, None, Some("first line\nsecond"), None, Some(30));
        board.duplicate(&id, 2);
        let back = Board::parse(&board.to_json());
        assert_eq!(back, board);
        assert_eq!(back.tasks[1].taken_min, 0);
        assert_eq!(Board::parse(""), Board::default());
        assert_eq!(Board::parse("# not json"), Board::default());
        let markdown = board.to_markdown("Plan");
        assert!(markdown.contains("## Today"));
        assert!(markdown.contains("- [ ] Write (est 01:30, taken 00:30)"));
        assert!(markdown.contains("  > first line"));
        assert_eq!(board.summary(), "2 pending tasks, Est: 2hr 30mins");
    }

    #[test]
    fn quick_add_falls_back_to_the_column_and_typed_estimate() {
        let mut board = Board::default();
        let id = board.quick_add("Plan 30m", "", Column::Backlog, 1).unwrap();
        assert_eq!(board.task(&id).unwrap().estimate_min, 30);
        let id = board
            .quick_add("Ship @today", "01:00", Column::Backlog, 1)
            .unwrap();
        let task = board.task(&id).unwrap();
        assert_eq!(
            (task.title.as_str(), task.column, task.estimate_min),
            ("Ship", Column::Today, 60)
        );
        assert!(board.quick_add("   ", "", Column::Backlog, 1).is_none());
    }
}
