//! Fast, cached search for "search everywhere" and quick open.
//!
//! * [`FileIndex`] holds a project's file list with lowercase keys computed
//!   once, so ranking 50k files per keystroke allocates nothing for the files
//!   that do not match.
//! * [`cached_index`] / [`load_index`] keep the last few indexes per project,
//!   so reopening the modal shows files at once.
//! * [`ContentSearcher`] reuses earlier `git grep` results when the user only
//!   extends a literal query.
use crate::files::ProjectFile;
use crate::search::project::{self, SearchMatch, SearchOptions, SearchResult};
use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, MutexGuard, OnceLock,
    },
    time::{Duration, Instant},
};

/// How many files are ranked between two looks at the cancel flag.
const CANCEL_STRIDE: usize = 1024;

// Score tiers for a token matched in the file name. They dwarf the per-char
// scores, so the kind of match decides first and its quality second.
const EXACT_NAME: i64 = 4000;
const EXACT_STEM: i64 = 3000;
const NAME_PREFIX: i64 = 2000;
const NAME_MATCH: i64 = 500;
// Shape bonuses for the chosen match positions (name or path).
const WORD_SUBSTRING: i64 = 800;
const MID_SUBSTRING: i64 = 300;
const ACRONYM: i64 = 150;
/// A path token (`src/ma`) that is the end of the path.
const PATH_SUFFIX: i64 = 600;
const RECENT_SLOTS: usize = 24;
const RECENT_STEP: i64 = 20;
/// Contiguous occurrences tried per token and text.
const MAX_OCCURRENCES: usize = 16;

// ---------------------------------------------------------------------------
// File index
// ---------------------------------------------------------------------------

struct Keys {
    /// `relative`, folded to lowercase with the same byte length.
    rel: Box<[u8]>,
    /// `name`, folded the same way.
    name: Box<[u8]>,
    /// Byte offset of `name` inside `relative` when it is its last segment.
    name_at: Option<usize>,
    /// Bitset of the bytes in `rel` and `name` (see [`bit`]).
    mask: u64,
}

/// A project's file list with precomputed lowercase keys, shared via Arc.
pub struct FileIndex {
    files: Vec<ProjectFile>,
    keys: Vec<Keys>,
    built_at: u64,
    by_path: OnceLock<Vec<u32>>,
    by_relative: OnceLock<Vec<u32>>,
}

impl std::fmt::Debug for FileIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileIndex")
            .field("files", &self.files.len())
            .field("built_at", &self.built_at)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileHit {
    /// Index into `FileIndex::files()`.
    pub index: usize,
    pub score: i64,
    /// UTF-8 byte ranges to highlight in `files()[index].name` (merged, sorted).
    pub name_ranges: Vec<Range<usize>>,
    /// UTF-8 byte ranges to highlight in `files()[index].relative` (merged, sorted).
    pub path_ranges: Vec<Range<usize>>,
}

/// Lowercase `ch` when that keeps its UTF-8 length, so folded keys stay byte
/// aligned with the original text and match offsets are highlight offsets.
fn fold_char(ch: char) -> char {
    if ch.is_ascii() {
        return ch.to_ascii_lowercase();
    }
    let mut lower = ch.to_lowercase();
    match (lower.next(), lower.next()) {
        (Some(c), None) if c.len_utf8() == ch.len_utf8() => c,
        _ => ch,
    }
}

fn fold(text: &str) -> Box<[u8]> {
    if text.is_ascii() {
        return text.as_bytes().to_ascii_lowercase().into_boxed_slice();
    }
    let folded: String = text.chars().map(fold_char).collect();
    debug_assert_eq!(folded.len(), text.len());
    folded.into_bytes().into_boxed_slice()
}

fn bit(b: u8) -> u64 {
    match b {
        b'a'..=b'z' => 1 << (b - b'a'),
        b'0'..=b'9' => 1 << (26 + b - b'0'),
        _ => 1 << (36 + b % 28),
    }
}

fn mask(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |m, &b| m | bit(b))
}

fn is_subsequence(needle: &[u8], hay: &[u8]) -> bool {
    let mut hay = hay.iter();
    needle.iter().all(|n| hay.any(|h| h == n))
}

/// First offset `>= from` where `hay` continues with `unit`. `unit` is whole
/// UTF-8 characters, so a hit always starts on a character boundary.
fn find_from(hay: &[u8], unit: &[u8], from: usize) -> Option<usize> {
    let first = *unit.first()?;
    let mut at = from;
    while at + unit.len() <= hay.len() {
        at += hay[at..].iter().position(|&b| b == first)?;
        if hay[at..].starts_with(unit) {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// Last offset whose `unit` ends at or before `end`.
fn rfind_before(hay: &[u8], unit: &[u8], end: usize) -> Option<usize> {
    let len = unit.len();
    if end < len {
        return None;
    }
    (0..=end - len).rev().find(|&at| hay[at..at + len] == *unit)
}

/// Bonus for a match starting at `at` in the original (unfolded) text.
fn boundary(orig: &[u8], at: usize) -> i64 {
    if at == 0 {
        return 30;
    }
    let (prev, cur) = (orig[at - 1], orig[at]);
    match prev {
        b'/' | b'\\' => 30,
        b'_' | b'-' | b'.' | b' ' => 24,
        _ if cur.is_ascii_uppercase() && prev.is_ascii_lowercase() => 28,
        _ if cur.is_ascii_digit() && prev.is_ascii_alphabetic() => 8,
        _ => 0,
    }
}

struct Token {
    /// Folded token bytes.
    bytes: Vec<u8>,
    /// (offset, len) of each character in `bytes`.
    units: Vec<(usize, usize)>,
    mask: u64,
    /// Contains `/`: matched against the relative path only.
    path: bool,
}

impl Token {
    fn unit(&self, ix: usize) -> &[u8] {
        let (at, len) = self.units[ix];
        &self.bytes[at..at + len]
    }
}

fn tokens(query: &str) -> Vec<Token> {
    query
        .split_whitespace()
        .map(|token| {
            let mut bytes = Vec::with_capacity(token.len());
            let mut units = Vec::with_capacity(token.len());
            for ch in token.chars() {
                let mut buf = [0; 4];
                let folded = fold_char(ch).encode_utf8(&mut buf);
                units.push((bytes.len(), folded.len()));
                bytes.extend_from_slice(folded.as_bytes());
            }
            Token {
                mask: mask(&bytes),
                path: token.contains('/'),
                bytes,
                units,
            }
        })
        .collect()
}

/// Per-character score of `positions` (start offsets of the token's chars).
/// `shape_div` scales the shape bonus down (2 for a name token that only
/// matched in the path, so a file name match stays ahead).
fn score_positions(orig: &[u8], positions: &[usize], token: &Token, shape_div: i64) -> i64 {
    let mut score = 0;
    let mut prev_end = 0;
    let mut contiguous = true;
    let mut acronym = true;
    for (ix, &at) in positions.iter().enumerate() {
        let bonus = boundary(orig, at);
        score += 16;
        if ix == 0 {
            score += bonus * 2;
        } else if at == prev_end {
            score += 14 + bonus / 2;
        } else {
            contiguous = false;
            score += bonus - 3 - (at - prev_end).min(32) as i64 / 4;
        }
        if bonus == 0 && (ix == 0 || at != prev_end) {
            acronym = false;
        }
        prev_end = at + token.units[ix].1;
    }
    let first = positions.first().map_or(0, |&at| boundary(orig, at));
    score
        + if contiguous && first > 0 {
            WORD_SUBSTRING
        } else if contiguous && token.units.len() >= 3 {
            MID_SUBSTRING
        } else if !contiguous && acronym {
            ACRONYM
        } else {
            0
        } / shape_div
}

/// Reused buffers, so ranking allocates nothing per file.
#[derive(Default)]
struct Scratch {
    best: Vec<usize>,
    cand: Vec<usize>,
    name: Vec<Range<usize>>,
    path: Vec<Range<usize>>,
}

/// Best placement of `token` in `key` (folded) / `orig`; positions in `best`.
fn best_match(
    key: &[u8],
    orig: &[u8],
    token: &Token,
    shape_div: i64,
    best: &mut Vec<usize>,
    cand: &mut Vec<usize>,
) -> Option<i64> {
    if token.bytes.is_empty() || !is_subsequence(&token.bytes, key) {
        return None;
    }
    let mut best_score: Option<i64> = None;
    let mut consider = |cand: &mut Vec<usize>, best: &mut Vec<usize>| {
        let score = score_positions(orig, cand, token, shape_div);
        if best_score.is_none_or(|b| score > b) {
            best_score = Some(score);
            std::mem::swap(best, cand);
        }
    };
    // Contiguous occurrences.
    let mut from = 0;
    for _ in 0..MAX_OCCURRENCES {
        let Some(at) = find_from(key, &token.bytes, from) else {
            break;
        };
        cand.clear();
        cand.extend(token.units.iter().map(|&(off, _)| at + off));
        consider(cand, best);
        from = at + token.units[0].1;
    }
    // fzf v1: the first match's end, then the shortest window before it.
    let mut end = 0;
    for ix in 0..token.units.len() {
        let at = find_from(key, token.unit(ix), end)?;
        end = at + token.units[ix].1;
    }
    let mut start = end;
    for ix in (0..token.units.len()).rev() {
        start = rfind_before(key, token.unit(ix), start)?;
    }
    cand.clear();
    let mut pos = start;
    for ix in 0..token.units.len() {
        let at = find_from(key, token.unit(ix), pos)?;
        cand.push(at);
        pos = at + token.units[ix].1;
    }
    consider(cand, best);
    // Greedy that prefers word starts (so `qo` lands on Quick|Open).
    cand.clear();
    let mut pos = 0;
    for ix in 0..token.units.len() {
        let len = token.units[ix].1;
        let unit = token.unit(ix);
        let plain = find_from(key, unit, pos)?;
        let mut pick = plain;
        let consecutive = ix > 0 && plain == pos;
        if !consecutive && boundary(orig, plain) == 0 {
            let rest = &token.bytes[token.units[ix].0 + len..];
            let mut from = plain + len;
            for _ in 0..8 {
                let Some(at) = find_from(key, unit, from) else {
                    break;
                };
                if boundary(orig, at) > 0 && is_subsequence(rest, &key[at + len..]) {
                    pick = at;
                    break;
                }
                from = at + len;
            }
        }
        cand.push(pick);
        pos = pick + len;
    }
    consider(cand, best);
    best_score
}

fn sorted_by(files: &[ProjectFile], key: fn(&ProjectFile) -> &str) -> Vec<u32> {
    let mut order: Vec<u32> = (0..files.len() as u32).collect();
    order.sort_unstable_by(|&a, &b| key(&files[a as usize]).cmp(key(&files[b as usize])));
    order
}

fn merged(ranges: &mut Vec<Range<usize>>) -> Vec<Range<usize>> {
    ranges.sort_unstable_by_key(|r| (r.start, r.end));
    let mut out: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        if let Some(last) = out.last_mut() {
            if range.start <= last.end {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        out.push(range);
    }
    out
}

impl FileIndex {
    pub fn new(files: Vec<ProjectFile>) -> Self {
        let keys = files
            .iter()
            .map(|f| {
                let rel = fold(&f.relative);
                let name = fold(&f.name);
                let name_at = f
                    .relative
                    .ends_with(f.name.as_str())
                    .then(|| f.relative.len() - f.name.len());
                Keys {
                    mask: mask(&rel) | mask(&name),
                    rel,
                    name,
                    name_at,
                }
            })
            .collect();
        Self {
            files,
            keys,
            built_at: crate::time::now_ms(),
            by_path: OnceLock::new(),
            by_relative: OnceLock::new(),
        }
    }

    pub fn files(&self) -> &[ProjectFile] {
        &self.files
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// When this index was built (ms since epoch, crate::time::now_ms()).
    pub fn built_at(&self) -> u64 {
        self.built_at
    }

    /// The file whose absolute `path` (or else `relative`) is `key`.
    pub fn position(&self, key: &str) -> Option<usize> {
        let by_path = self
            .by_path
            .get_or_init(|| sorted_by(&self.files, |f| &f.path));
        if let Ok(at) =
            by_path.binary_search_by(|&ix| self.files[ix as usize].path.as_str().cmp(key))
        {
            return Some(by_path[at] as usize);
        }
        let by_relative = self
            .by_relative
            .get_or_init(|| sorted_by(&self.files, |f| &f.relative));
        by_relative
            .binary_search_by(|&ix| self.files[ix as usize].relative.as_str().cmp(key))
            .ok()
            .map(|at| by_relative[at] as usize)
    }

    fn recent_indexes(&self, recent: &[String]) -> Vec<usize> {
        let mut out = Vec::with_capacity(recent.len());
        for key in recent {
            if let Some(ix) = self.position(key) {
                if !out.contains(&ix) {
                    out.push(ix);
                }
            }
        }
        out
    }

    fn score_file(
        &self,
        ix: usize,
        tokens: &[Token],
        s: &mut Scratch,
        ranges: bool,
    ) -> Option<i64> {
        let file = &self.files[ix];
        let keys = &self.keys[ix];
        let (name, rel) = (file.name.as_bytes(), file.relative.as_bytes());
        let mut total = -(rel.len() as i64);
        for token in tokens {
            if !token.path {
                if let Some(score) =
                    best_match(&keys.name, name, token, 1, &mut s.best, &mut s.cand)
                {
                    let tier = if *keys.name == *token.bytes {
                        EXACT_NAME
                    } else if keys.name.starts_with(&token.bytes) {
                        if keys.name[token.bytes.len()] == b'.' {
                            EXACT_STEM
                        } else {
                            NAME_PREFIX
                        }
                    } else {
                        0
                    };
                    total += score + tier + NAME_MATCH - 2 * name.len() as i64;
                    if ranges {
                        for (u, &at) in s.best.iter().enumerate() {
                            let r = at..at + token.units[u].1;
                            if let Some(off) = keys.name_at {
                                s.path.push(r.start + off..r.end + off);
                            }
                            s.name.push(r);
                        }
                    }
                    continue;
                }
            }
            let shape_div = if token.path { 1 } else { 2 };
            let score = best_match(&keys.rel, rel, token, shape_div, &mut s.best, &mut s.cand)?;
            total += score;
            if token.path && keys.rel.ends_with(&token.bytes) {
                total += PATH_SUFFIX;
            }
            if ranges {
                for (u, &at) in s.best.iter().enumerate() {
                    let r = at..at + token.units[u].1;
                    if let Some(off) = keys.name_at {
                        if r.start >= off {
                            s.name.push(r.start - off..r.end - off);
                        }
                    }
                    s.path.push(r);
                }
            }
        }
        Some(total)
    }

    /// Fuzzy-ranks files for `query`. An empty query gives the `recent` files
    /// that exist in the index, in `recent` order. `recent` entries are
    /// absolute paths (`ProjectFile::path`) or relative paths.
    ///
    /// Whitespace-separated tokens must all match. A token with `/` matches
    /// the relative path; others prefer the file name and fall back to the
    /// path. Best first; ties by shorter relative path, then lexicographic.
    /// Returns an empty Vec once `cancel` is set.
    pub fn search(
        &self,
        query: &str,
        recent: &[String],
        limit: usize,
        cancel: &AtomicBool,
    ) -> Vec<FileHit> {
        if limit == 0 {
            return Vec::new();
        }
        let tokens = tokens(query);
        let recent = self.recent_indexes(recent);
        if tokens.is_empty() {
            return recent
                .into_iter()
                .take(limit)
                .map(|index| FileHit {
                    index,
                    score: 0,
                    name_ranges: Vec::new(),
                    path_ranges: Vec::new(),
                })
                .collect();
        }
        let boost: HashMap<usize, i64> = recent
            .iter()
            .take(RECENT_SLOTS)
            .enumerate()
            .map(|(rank, &ix)| (ix, (RECENT_SLOTS - rank) as i64 * RECENT_STEP))
            .collect();
        let need = tokens.iter().fold(0, |m, t| m | t.mask);
        let mut scratch = Scratch::default();
        let mut scored: Vec<(i64, u32)> = Vec::new();
        for (ix, keys) in self.keys.iter().enumerate() {
            if ix % CANCEL_STRIDE == 0 && cancel.load(Ordering::Acquire) {
                return Vec::new();
            }
            if keys.mask & need != need {
                continue;
            }
            let possible = tokens.iter().all(|t| {
                is_subsequence(&t.bytes, &keys.rel)
                    || (!t.path && keys.name_at.is_none() && is_subsequence(&t.bytes, &keys.name))
            });
            if !possible {
                continue;
            }
            if let Some(score) = self.score_file(ix, &tokens, &mut scratch, false) {
                scored.push((score + boost.get(&ix).copied().unwrap_or(0), ix as u32));
            }
        }
        if cancel.load(Ordering::Acquire) {
            return Vec::new();
        }
        let rel = |ix: u32| self.files[ix as usize].relative.as_str();
        let order = |a: &(i64, u32), b: &(i64, u32)| {
            b.0.cmp(&a.0)
                .then_with(|| rel(a.1).len().cmp(&rel(b.1).len()))
                .then_with(|| rel(a.1).cmp(rel(b.1)))
                .then_with(|| a.1.cmp(&b.1))
        };
        if scored.len() > limit {
            scored.select_nth_unstable_by(limit - 1, order);
            scored.truncate(limit);
        }
        scored.sort_unstable_by(order);
        scored
            .into_iter()
            .map(|(score, ix)| {
                let ix = ix as usize;
                self.score_file(ix, &tokens, &mut scratch, true);
                FileHit {
                    index: ix,
                    score,
                    name_ranges: merged(&mut scratch.name),
                    path_ranges: merged(&mut scratch.path),
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Index cache
// ---------------------------------------------------------------------------

const INDEX_CACHE_SIZE: usize = 8;

struct IndexEntry {
    cwd: String,
    include_ignored: bool,
    index: Arc<FileIndex>,
    used: u64,
}

struct IndexCache {
    entries: Vec<IndexEntry>,
    tick: u64,
    capacity: usize,
}

impl IndexCache {
    const fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::new(),
            tick: 0,
            capacity,
        }
    }

    fn get(&mut self, cwd: &str, include_ignored: bool) -> Option<Arc<FileIndex>> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.cwd == cwd && e.include_ignored == include_ignored)?;
        entry.used = tick;
        Some(entry.index.clone())
    }

    fn put(&mut self, cwd: &str, include_ignored: bool, index: Arc<FileIndex>) {
        self.tick += 1;
        self.entries
            .retain(|e| !(e.cwd == cwd && e.include_ignored == include_ignored));
        self.entries.push(IndexEntry {
            cwd: cwd.to_owned(),
            include_ignored,
            index,
            used: self.tick,
        });
        while self.entries.len() > self.capacity {
            let oldest = (0..self.entries.len())
                .min_by_key(|&ix| self.entries[ix].used)
                .unwrap_or(0);
            self.entries.remove(oldest);
        }
    }

    fn remove(&mut self, cwd: &str) {
        self.entries.retain(|e| e.cwd != cwd);
    }
}

static INDEXES: Mutex<IndexCache> = Mutex::new(IndexCache::new(INDEX_CACHE_SIZE));

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn cwd_key(cwd: &str) -> &str {
    let trimmed = cwd.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        cwd
    } else {
        trimmed
    }
}

/// Process-wide cache of file indexes keyed by (cwd, include_ignored); the 8
/// most recently used are kept.
pub fn cached_index(cwd: &str, include_ignored: bool) -> Option<Arc<FileIndex>> {
    lock(&INDEXES).get(cwd_key(cwd), include_ignored)
}

/// Lists the project, builds an index, caches and returns it. On cancel
/// returns Err("cancelled") and leaves the cache alone.
pub fn load_index(
    cwd: &str,
    include_ignored: bool,
    cancel: &AtomicBool,
) -> Result<Arc<FileIndex>, String> {
    let cancelled = || cancel.load(Ordering::Acquire);
    if cancelled() {
        return Err("cancelled".into());
    }
    let files = crate::files::list_project_files_with(cwd, Some(cancel), include_ignored)?;
    if cancelled() {
        return Err("cancelled".into());
    }
    let index = Arc::new(FileIndex::new(files));
    if cancelled() {
        return Err("cancelled".into());
    }
    lock(&INDEXES).put(cwd_key(cwd), include_ignored, index.clone());
    Ok(index)
}

/// Drops cached indexes for `cwd` (both include_ignored variants).
pub fn invalidate_index(cwd: &str) {
    lock(&INDEXES).remove(cwd_key(cwd));
}

// ---------------------------------------------------------------------------
// Locations
// ---------------------------------------------------------------------------

fn number(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn found(path: &str, location: Option<(u32, u32)>) -> Option<(&str, Option<(u32, u32)>)> {
    let path = path.trim_end();
    (!path.is_empty()).then_some((path, location))
}

/// `"42"`, or `"42<sep>7"` with any of `seps`.
fn line_col(text: &str, seps: &[char]) -> Option<(u32, u32)> {
    match text.split_once(seps) {
        Some((line, col)) => Some((number(line)?, number(col)?)),
        None => Some((number(text)?, 1)),
    }
}

/// Splits a trailing location off a query: `"src/main.rs:42"` =>
/// `("src/main.rs", Some((42, 1)))`, `"main.rs:42:7"` => `("main.rs",
/// Some((42, 7)))`, `"foo"` => `("foo", None)`, `"a:b"` => `("a:b", None)`.
/// Also accepts `main.rs(42)`, `main.rs(42,7)`, `main.rs#L42`, `#L42C7`,
/// `#L42-L50` (start line) and a trailing `:` as grep prints it.
/// The column defaults to 1. Trims whitespace.
pub fn split_location(query: &str) -> (&str, Option<(u32, u32)>) {
    let query = query.trim();
    if let Some((path, loc)) = query.rsplit_once("#L") {
        let loc = loc.split_once("-").map_or(loc, |(start, _)| start);
        if let Some(hit) = line_col(loc, &['C', 'c']).and_then(|l| found(path, Some(l))) {
            return hit;
        }
    }
    if let Some(inner) = query.strip_suffix(')') {
        if let Some((path, loc)) = inner.rsplit_once('(') {
            if let Some(hit) = line_col(loc.trim(), &[',', ':']).and_then(|l| found(path, Some(l)))
            {
                return hit;
            }
        }
    }
    let body = query.strip_suffix(':').unwrap_or(query);
    if let Some((rest, last)) = body.rsplit_once(':') {
        if let Some(last) = number(last) {
            if let Some((path, line)) = rest.rsplit_once(':') {
                if let Some(hit) = number(line).and_then(|line| found(path, Some((line, last)))) {
                    return hit;
                }
            }
            if let Some(hit) = found(rest, Some((last, 1))) {
                return hit;
            }
        }
    }
    (query, None)
}

// ---------------------------------------------------------------------------
// Content search
// ---------------------------------------------------------------------------

const CONTENT_CACHE_SIZE: usize = 16;
const CONTENT_TTL: Duration = Duration::from_secs(20);

struct ContentEntry {
    root: PathBuf,
    /// The options with a trimmed query and no `search_id`.
    key: SearchOptions,
    result: SearchResult,
    stored: Instant,
    used: u64,
}

struct ContentCache {
    entries: Vec<ContentEntry>,
    tick: u64,
}

/// Content search with result reuse. Keeps the 16 most recently used results
/// for 20 s, keyed by the root and every [`SearchOptions`] field except
/// `search_id` (a per-request id that would defeat the cache); the query is
/// compared trimmed, as `search_project` uses it.
///
/// * Same options as a cached entry: the cached result.
/// * A literal, non-whole-word query that contains a cached query (folded to
///   lowercase when case-insensitive) with the same other options and a
///   complete (not truncated) result: the cached matches are filtered in
///   memory and their columns recomputed, without running `git grep`.
/// * Otherwise `search_project`, cached unless cancelled.
///
/// A preview longer than the clip window is only a window of its line, so a
/// refined search over such a match keeps it only when the window still holds
/// the new query, and reports the first occurrence inside the window. An
/// occurrence outside the window is missed; this is accepted, as a refined
/// query almost always still matches around the original hit.
pub struct ContentSearcher {
    cache: Mutex<ContentCache>,
    capacity: usize,
    ttl: Duration,
}

impl Default for ContentSearcher {
    fn default() -> Self {
        Self::new()
    }
}

fn content_key(options: &SearchOptions) -> SearchOptions {
    SearchOptions {
        query: options.query.trim().to_owned(),
        search_id: String::new(),
        ..options.clone()
    }
}

fn same_except_query(a: &SearchOptions, b: &SearchOptions) -> bool {
    a.cwd == b.cwd
        && a.case_sensitive == b.case_sensitive
        && a.whole_word == b.whole_word
        && a.regex == b.regex
        && a.include == b.include
        && a.exclude == b.exclude
        && a.include_ignored == b.include_ignored
}

/// `m` narrowed to `matcher` (the refined literal query), or None.
fn refine_match(m: &SearchMatch, matcher: &regex::Regex) -> Option<SearchMatch> {
    let preview = m.preview.as_str();
    let mut lead = 0;
    let mut body = preview;
    // `clip_preview` keeps short lines whole; a longer preview is a window of
    // PREVIEW_CHARS chars starting `at` chars into the line, with "…" where
    // text was cut.
    if preview.chars().count() > project::PREVIEW_CHARS {
        let at = (m.column.max(1) as usize - 1).saturating_sub(project::PREVIEW_LEAD);
        if at > 0 {
            body = body.strip_prefix('…').unwrap_or(body);
            lead = at;
        }
        if body.chars().count() > project::PREVIEW_CHARS {
            body = body.strip_suffix('…').unwrap_or(body);
        }
    }
    let found = matcher.find(body)?;
    Some(SearchMatch {
        column: (lead + body[..found.start()].chars().count() + 1) as u32,
        ..m.clone()
    })
}

impl ContentCache {
    fn expire(&mut self, ttl: Duration) {
        self.entries.retain(|e| e.stored.elapsed() < ttl);
    }

    fn exact(&mut self, root: &Path, key: &SearchOptions) -> Option<SearchResult> {
        self.tick += 1;
        let tick = self.tick;
        let entry = self
            .entries
            .iter_mut()
            .find(|e| e.root == root && e.key == *key)?;
        entry.used = tick;
        Some(entry.result.clone())
    }

    fn refine(&mut self, root: &Path, key: &SearchOptions) -> Option<SearchResult> {
        if key.regex || key.whole_word {
            return None;
        }
        let fold = |text: &str| {
            if key.case_sensitive {
                text.to_owned()
            } else {
                text.to_lowercase()
            }
        };
        let query = fold(&key.query);
        self.tick += 1;
        let tick = self.tick;
        let base = self
            .entries
            .iter_mut()
            .filter(|e| {
                e.root == root
                    && !e.result.truncated
                    && !e.key.query.is_empty()
                    && same_except_query(&e.key, key)
                    && query.contains(&fold(&e.key.query))
            })
            .max_by_key(|e| e.key.query.len())?;
        base.used = tick;
        let matcher = regex::RegexBuilder::new(&regex::escape(&key.query))
            .case_insensitive(!key.case_sensitive)
            .build()
            .ok()?;
        let matches = base
            .result
            .matches
            .iter()
            .filter_map(|m| refine_match(m, &matcher))
            .collect();
        Some(SearchResult {
            matches,
            truncated: false,
        })
    }

    fn put(&mut self, root: &Path, key: SearchOptions, result: SearchResult, capacity: usize) {
        self.tick += 1;
        self.entries.retain(|e| !(e.root == root && e.key == key));
        self.entries.push(ContentEntry {
            root: root.to_path_buf(),
            key,
            result,
            stored: Instant::now(),
            used: self.tick,
        });
        while self.entries.len() > capacity {
            let oldest = (0..self.entries.len())
                .min_by_key(|&ix| self.entries[ix].used)
                .unwrap_or(0);
            self.entries.remove(oldest);
        }
    }
}

impl ContentSearcher {
    pub const fn new() -> Self {
        Self::with_limits(CONTENT_CACHE_SIZE, CONTENT_TTL)
    }

    /// A searcher keeping `capacity` results for `ttl`.
    pub const fn with_limits(capacity: usize, ttl: Duration) -> Self {
        Self {
            cache: Mutex::new(ContentCache {
                entries: Vec::new(),
                tick: 0,
            }),
            capacity,
            ttl,
        }
    }

    pub fn search(
        &self,
        root: &Path,
        options: &SearchOptions,
        cancel: &AtomicBool,
    ) -> Result<SearchResult, String> {
        let key = content_key(options);
        if key.query.is_empty() || cancel.load(Ordering::Acquire) {
            return project::search_project(root, options, cancel);
        }
        {
            let mut cache = lock(&self.cache);
            cache.expire(self.ttl);
            if let Some(hit) = cache.exact(root, &key) {
                return Ok(hit);
            }
            if let Some(refined) = cache.refine(root, &key) {
                cache.put(root, key, refined.clone(), self.capacity);
                return Ok(refined);
            }
        }
        let result = project::search_project(root, options, cancel)?;
        if !cancel.load(Ordering::Acquire) {
            lock(&self.cache).put(root, key, result.clone(), self.capacity);
        }
        Ok(result)
    }

    pub fn clear(&self) {
        lock(&self.cache).entries.clear();
    }
}

static CONTENT_SEARCHER: ContentSearcher = ContentSearcher::new();

/// One shared searcher for the process.
pub fn content_searcher() -> &'static ContentSearcher {
    &CONTENT_SEARCHER
}

/// Byte ranges of the occurrences of `query` in `text` — a literal, matched
/// case-insensitively unless `case_sensitive`, or a regex when `regex` (an
/// invalid regex gives none). The query is trimmed like `search_project`
/// trims it; empty matches are skipped; at most 16 ranges.
pub fn match_ranges(
    text: &str,
    query: &str,
    case_sensitive: bool,
    regex: bool,
) -> Vec<Range<usize>> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    let pattern = if regex {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    let Ok(matcher) = regex::RegexBuilder::new(&pattern)
        .case_insensitive(!case_sensitive)
        .build()
    else {
        return Vec::new();
    };
    matcher
        .find_iter(text)
        .filter(|m| !m.is_empty())
        .take(16)
        .map(|m| m.range())
        .collect()
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn file(relative: &str) -> ProjectFile {
        ProjectFile {
            name: relative.rsplit('/').next().unwrap().to_owned(),
            path: format!("/repo/{relative}"),
            relative: relative.to_owned(),
        }
    }
    fn index(paths: &[&str]) -> FileIndex {
        FileIndex::new(paths.iter().map(|p| file(p)).collect())
    }
    fn go() -> AtomicBool {
        AtomicBool::new(false)
    }
    fn ranked(ix: &FileIndex, query: &str) -> Vec<String> {
        ranked_with(ix, query, &[])
    }
    fn ranked_with(ix: &FileIndex, query: &str, recent: &[String]) -> Vec<String> {
        ix.search(query, recent, 100, &go())
            .into_iter()
            .map(|h| ix.files()[h.index].relative.clone())
            .collect()
    }
    fn assert_ranges_valid(ix: &FileIndex, hits: &[FileHit]) {
        for hit in hits {
            let f = &ix.files()[hit.index];
            for (text, ranges) in [(&f.name, &hit.name_ranges), (&f.relative, &hit.path_ranges)] {
                let mut last = 0;
                for r in ranges {
                    assert!(r.start < r.end && r.start >= last, "{ranges:?}");
                    assert!(r.end <= text.len(), "{text} {r:?}");
                    assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
                    last = r.end + 1; // merged: no touching ranges
                }
            }
        }
    }

    #[test]
    fn exact_beats_prefix_beats_substring_beats_scattered() {
        let ix = index(&[
            "src/fxoxo.rs",
            "src/xfoo.rs",
            "src/foobar.rs",
            "src/foo.rs",
            "lib/foo/mod.rs",
        ]);
        assert_eq!(
            ranked(&ix, "foo"),
            [
                "src/foo.rs",
                "src/foobar.rs",
                "src/xfoo.rs",
                "src/fxoxo.rs",
                "lib/foo/mod.rs"
            ]
        );
        let ix = index(&["a/xfoo.rs", "a/foo.rs.bak", "a/foo.rs", "a/f_o_o.rs"]);
        assert_eq!(
            ranked(&ix, "foo.rs"),
            ["a/foo.rs", "a/foo.rs.bak", "a/xfoo.rs", "a/f_o_o.rs"]
        );
    }

    #[test]
    fn scattered_query_finds_the_name_and_highlights_it() {
        let ix = index(&[
            "README.md",
            "src/domain/rust.txt",
            "src/main.rs",
            "src/remains.txt",
        ]);
        let hits = ix.search("mainrs", &[], 10, &go());
        assert_eq!(ix.files()[hits[0].index].relative, "src/main.rs");
        assert_eq!(hits[0].name_ranges, [0..4, 5..7]);
        assert_eq!(hits[0].path_ranges, [4..8, 9..11]);
        assert_ranges_valid(&ix, &hits);
    }

    #[test]
    fn slash_tokens_match_the_path() {
        let ix = index(&[
            "lib/main.rs",
            "src/lib.rs",
            "src/main.rs",
            "other/src/xmain.rs",
        ]);
        let hits = ix.search("src/ma", &[], 10, &go());
        let names: Vec<_> = hits
            .iter()
            .map(|h| ix.files()[h.index].relative.as_str())
            .collect();
        assert_eq!(names[0], "src/main.rs");
        assert!(!names.contains(&"lib/main.rs") && !names.contains(&"src/lib.rs"));
        assert_eq!(hits[0].path_ranges, [0..6]);
        assert_eq!(hits[0].name_ranges, [0..2]);
        assert_ranges_valid(&ix, &hits);
    }

    #[test]
    fn all_tokens_must_match() {
        let ix = index(&["src/main.rs", "tests/main_test.rs", "src/test.rs"]);
        assert_eq!(ranked(&ix, "main test"), ["tests/main_test.rs"]);
        assert_eq!(ranked(&ix, "  test   main "), ["tests/main_test.rs"]);
        assert!(ranked(&ix, "main zzz").is_empty());
        // A directory token plus a name token.
        let ix = index(&[
            "crates/core/lib.rs",
            "crates/gpui/lib.rs",
            "crates/gpui/app.rs",
        ]);
        assert_eq!(ranked(&ix, "gpui lib"), ["crates/gpui/lib.rs"]);
    }

    #[test]
    fn camel_case_initials_rank_high() {
        let ix = index(&[
            "docs/question_of_the_day.md",
            "src/quota.rs",
            "src/QuickOpen.rs",
            "src/aquo.rs",
        ]);
        assert_eq!(ranked(&ix, "QO")[0], "src/QuickOpen.rs");
        assert_eq!(ranked(&ix, "qo")[0], "src/QuickOpen.rs");
        let hits = ix.search("QO", &[], 10, &go());
        assert_eq!(hits[0].name_ranges, [0..1, 5..6]);
        // Even next to a name holding "qo" contiguously it stays near the top.
        let ix = index(&["src/aqo.txt", "src/QuickOpen.rs", "src/quota.rs"]);
        assert!(ranked(&ix, "QO")[..2].contains(&"src/QuickOpen.rs".to_owned()));
        let ix = index(&[
            "src/search_everywhere.rs",
            "src/session_explorer.rs",
            "src/seven.rs",
        ]);
        assert_eq!(ranked(&ix, "se")[0], "src/seven.rs", "prefix first");
        assert_eq!(ranked(&ix, "sev")[0], "src/seven.rs");
        assert_eq!(ranked(&ix, "sevw")[0], "src/search_everywhere.rs");
    }

    #[test]
    fn shorter_paths_win_ties_then_lexicographic() {
        let ix = index(&["deep/nested/dir/util.rs", "b/util.rs", "a/util.rs"]);
        assert_eq!(
            ranked(&ix, "util"),
            ["a/util.rs", "b/util.rs", "deep/nested/dir/util.rs"]
        );
    }

    #[test]
    fn unicode_names_do_not_panic_and_ranges_are_char_boundaries() {
        let ix = index(&[
            "src/日本語/ファイル.rs",
            "docs/Ärger_ÉCOLE.md",
            "İstanbul.txt",
            "\u{212A}elvin.txt",
            "emoji/🙂smile🙂.txt",
            "mixed/straße.rs",
        ]);
        for query in [
            "ファ",
            "日本 rs",
            "äé",
            "écol",
            "ÉCOLE",
            "i",
            "k",
            "kelvin",
            "🙂",
            "smile",
            "ß",
            "STRASSE",
            "s/t",
            "é/",
            "ü",
            "日/フ",
        ] {
            let hits = ix.search(query, &[], 10, &go());
            assert_ranges_valid(&ix, &hits);
        }
        assert_eq!(ranked(&ix, "ファ"), ["src/日本語/ファイル.rs"]);
        assert_eq!(ranked(&ix, "äécol"), ["docs/Ärger_ÉCOLE.md"]);
        assert_eq!(ranked(&ix, "smile"), ["emoji/🙂smile🙂.txt"]);
        let hit = &ix.search("ファ", &[], 1, &go())[0];
        assert_eq!(
            &ix.files()[hit.index].name[hit.name_ranges[0].clone()],
            "ファ"
        );
    }

    #[test]
    fn recent_files_get_a_boost_and_fill_the_empty_query() {
        let ix = index(&["a/util.rs", "b/util.rs", "c/util.rs", "lib.rs"]);
        assert_eq!(ranked(&ix, "util")[0], "a/util.rs");
        let recent = vec!["/repo/c/util.rs".to_owned(), "b/util.rs".to_owned()];
        assert_eq!(
            ranked_with(&ix, "util", &recent),
            ["c/util.rs", "b/util.rs", "a/util.rs"]
        );
        // A recent file still needs to match.
        assert!(ranked_with(&ix, "lib", &recent) == ["lib.rs"]);
        let recent = vec![
            "/repo/lib.rs".to_owned(),
            "/elsewhere/gone.rs".to_owned(),
            "/repo/b/util.rs".to_owned(),
            "/repo/lib.rs".to_owned(),
            "a/util.rs".to_owned(),
        ];
        assert_eq!(
            ranked_with(&ix, "   ", &recent),
            ["lib.rs", "b/util.rs", "a/util.rs"]
        );
        let hits = ix.search("", &recent, 2, &go());
        assert_eq!(hits.len(), 2);
        assert!(hits
            .iter()
            .all(|h| h.name_ranges.is_empty() && h.path_ranges.is_empty()));
        assert!(ix.search("", &[], 10, &go()).is_empty());
        assert!(ix.search("util", &[], 0, &go()).is_empty());
        assert_eq!(ix.search("util", &[], 2, &go()).len(), 2);
    }

    #[test]
    fn cancel_returns_nothing() {
        let ix = index(&["a/util.rs", "b/util.rs"]);
        assert!(ix
            .search("util", &[], 10, &AtomicBool::new(true))
            .is_empty());
        assert_eq!(ix.len(), 2);
        assert!(!ix.is_empty());
        assert!(ix.built_at() > 0);
        assert!(FileIndex::new(vec![]).is_empty());
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        assert_eq!(
            load_index(&cwd, false, &AtomicBool::new(true)).unwrap_err(),
            "cancelled"
        );
        assert!(cached_index(&cwd, false).is_none());
    }

    #[test]
    fn fifty_thousand_files_rank_quickly() {
        let words = [
            "search", "editor", "quick", "open", "panel", "view", "model", "store", "git", "diff",
            "terminal", "theme", "render", "layout", "buffer", "syntax",
        ];
        let files: Vec<ProjectFile> = (0..50_000)
            .map(|i| {
                let a = words[i % words.len()];
                let b = words[(i / 16) % words.len()];
                let c = words[(i / 256) % words.len()];
                file(&format!("crates/{c}/src/{b}/{a}_{b}{i}.rs"))
            })
            .collect();
        let started = Instant::now();
        let ix = FileIndex::new(files);
        let built = started.elapsed();
        let mut slowest = Duration::ZERO;
        for query in [
            "qop", "edit", "render", "srcview", "gdiff", "xyz", "q/o", "tmlay",
        ] {
            let started = Instant::now();
            let hits = ix.search(query, &[], 50, &go());
            let took = started.elapsed();
            slowest = slowest.max(took);
            eprintln!("{query:>8}: {} hits in {took:?}", hits.len());
        }
        eprintln!("built 50k index in {built:?}, slowest query {slowest:?}");
        assert!(slowest < Duration::from_millis(250), "{slowest:?}");
    }

    #[test]
    fn index_cache_is_lru_and_invalidates() {
        let mut cache = IndexCache::new(3);
        for cwd in ["/a", "/b", "/c"] {
            cache.put(cwd, false, Arc::new(FileIndex::new(vec![])));
        }
        assert!(cache.get("/a", false).is_some()); // /b is now the oldest
        cache.put("/d", false, Arc::new(FileIndex::new(vec![])));
        assert!(cache.get("/b", false).is_none());
        assert!(cache.get("/a", false).is_some() && cache.get("/c", false).is_some());
        assert!(
            cache.get("/a", true).is_none(),
            "keyed by include_ignored too"
        );
        cache.put("/a", true, Arc::new(FileIndex::new(vec![])));
        cache.remove("/a");
        assert!(cache.get("/a", false).is_none() && cache.get("/a", true).is_none());

        // The process-wide cache, on a real folder.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        assert!(cached_index(&cwd, false).is_none());
        let loaded = load_index(&cwd, false, &go()).unwrap();
        assert!(loaded.files().iter().any(|f| f.relative == "src/main.rs"));
        let cached = cached_index(&cwd, false).unwrap();
        assert!(Arc::ptr_eq(&loaded, &cached));
        assert!(Arc::ptr_eq(
            &cached_index(&format!("{cwd}/"), false).unwrap(),
            &loaded
        ));
        assert!(cached_index(&cwd, true).is_none());
        load_index(&cwd, true, &go()).unwrap();
        invalidate_index(&cwd);
        assert!(cached_index(&cwd, false).is_none() && cached_index(&cwd, true).is_none());
    }

    #[test]
    fn split_location_cases() {
        assert_eq!(
            split_location("src/main.rs:42"),
            ("src/main.rs", Some((42, 1)))
        );
        assert_eq!(split_location("main.rs:42:7"), ("main.rs", Some((42, 7))));
        assert_eq!(split_location("main.rs:42:7:"), ("main.rs", Some((42, 7))));
        assert_eq!(split_location("main.rs:42:"), ("main.rs", Some((42, 1))));
        assert_eq!(split_location("foo"), ("foo", None));
        assert_eq!(split_location("a:b"), ("a:b", None));
        assert_eq!(split_location("a:b:3"), ("a:b", Some((3, 1))));
        assert_eq!(
            split_location("  main.rs(42)  "),
            ("main.rs", Some((42, 1)))
        );
        assert_eq!(split_location("main.rs(42,7)"), ("main.rs", Some((42, 7))));
        assert_eq!(split_location("main.rs#L42"), ("main.rs", Some((42, 1))));
        assert_eq!(split_location("main.rs#L42C3"), ("main.rs", Some((42, 3))));
        assert_eq!(
            split_location("main.rs#L42-L50"),
            ("main.rs", Some((42, 1)))
        );
        assert_eq!(split_location("main.rs #L9"), ("main.rs", Some((9, 1))));
        assert_eq!(split_location(" src/main.rs "), ("src/main.rs", None));
        assert_eq!(split_location(":42"), (":42", None));
        assert_eq!(split_location("#L42"), ("#L42", None));
        assert_eq!(split_location("f(x)"), ("f(x)", None));
        assert_eq!(
            split_location("main.rs:99999999999"),
            ("main.rs:99999999999", None)
        );
        assert_eq!(split_location(""), ("", None));
    }

    #[test]
    fn match_ranges_cases() {
        assert_eq!(
            match_ranges("Foo foo FOO", "foo", false, false),
            [0..3, 4..7, 8..11]
        );
        assert_eq!(match_ranges("Foo foo FOO", "foo", true, false), [4..7]);
        assert_eq!(match_ranges("a.b axb", "a.b", false, false), [0..3]);
        assert_eq!(match_ranges("a.b axb", "a.b", false, true), [0..3, 4..7]);
        assert!(match_ranges("abc", "(", false, true).is_empty());
        assert!(match_ranges("abc", "", false, false).is_empty());
        assert!(
            match_ranges("abc", "x*", false, true).is_empty(),
            "empty matches skipped"
        );
        assert_eq!(match_ranges(&"ab".repeat(40), "a", false, false).len(), 16);
        let text = "é🙂 Café";
        for r in match_ranges(text, "É", false, false) {
            assert!(text.is_char_boundary(r.start) && text.is_char_boundary(r.end));
        }
        assert_eq!(match_ranges(text, "café", false, false), [7..12]);
    }

    fn git(root: &Path, args: &[&str]) {
        assert!(std::process::Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap()
            .status
            .success());
    }
    fn options(query: &str) -> SearchOptions {
        SearchOptions {
            cwd: String::new(),
            query: query.into(),
            case_sensitive: false,
            whole_word: false,
            regex: false,
            include: None,
            exclude: None,
            search_id: String::new(),
            include_ignored: false,
        }
    }
    fn repo(files: &[(&str, String)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "--quiet"]);
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        git(dir.path(), &["add", "."]);
        dir
    }
    fn lines(result: &SearchResult) -> Vec<(String, u32, u32)> {
        result
            .matches
            .iter()
            .map(|m| (m.relative.clone(), m.line, m.column))
            .collect()
    }

    #[test]
    fn extending_a_literal_query_filters_the_cached_result() {
        let dir = repo(&[(
            "a.txt",
            "hello world\nsay Hello there\nnothing\n".to_owned(),
        )]);
        let root = dir.path();
        let searcher = ContentSearcher::new();
        let first = searcher.search(root, &options("hello"), &go()).unwrap();
        assert_eq!(
            lines(&first),
            [("a.txt".into(), 1, 1), ("a.txt".into(), 2, 5)]
        );
        // Without the file, only the cache can answer.
        std::fs::remove_file(root.join("a.txt")).unwrap();
        assert!(project::search_project(root, &options("hello"), &go())
            .unwrap()
            .matches
            .is_empty());
        let same = searcher
            .search(
                root,
                &SearchOptions {
                    search_id: "another-request".into(),
                    ..options("  hello ")
                },
                &go(),
            )
            .unwrap();
        assert_eq!(lines(&same), lines(&first), "search_id and padding ignored");
        let refined = searcher.search(root, &options("Hello T"), &go()).unwrap();
        assert_eq!(lines(&refined), [("a.txt".into(), 2, 5)]);
        assert!(!refined.truncated);
        let refined = searcher.search(root, &options("lo wor"), &go()).unwrap();
        assert!(
            refined.matches.is_empty(),
            "not an extension of a cached query"
        );
        let refined = searcher.search(root, &options("o wor"), &go()).unwrap();
        assert!(refined.matches.is_empty());
        let refined = searcher.search(root, &options("hello w"), &go()).unwrap();
        assert_eq!(lines(&refined), [("a.txt".into(), 1, 1)]);
        let refined = searcher.search(root, &options("llo wo"), &go()).unwrap();
        assert!(refined.matches.is_empty(), "contains, not extends: re-runs");
        // Case-sensitive searches never reuse case-insensitive results.
        let sensitive = SearchOptions {
            case_sensitive: true,
            ..options("Hello there")
        };
        assert!(searcher
            .search(root, &sensitive, &go())
            .unwrap()
            .matches
            .is_empty());
        // Regex and whole-word searches always run git grep.
        let regex = SearchOptions {
            regex: true,
            ..options("hello.*")
        };
        assert!(searcher
            .search(root, &regex, &go())
            .unwrap()
            .matches
            .is_empty());
        let word = SearchOptions {
            whole_word: true,
            ..options("hello")
        };
        assert!(searcher
            .search(root, &word, &go())
            .unwrap()
            .matches
            .is_empty());
        // Different filters do not reuse.
        let filtered = SearchOptions {
            include: Some("*.txt".into()),
            ..options("hello")
        };
        assert!(searcher
            .search(root, &filtered, &go())
            .unwrap()
            .matches
            .is_empty());
        searcher.clear();
        assert!(searcher
            .search(root, &options("hello"), &go())
            .unwrap()
            .matches
            .is_empty());
    }

    #[test]
    fn content_reuse_skips_truncated_expired_and_cancelled_results() {
        let dir = repo(&[
            ("many.txt", "needle\n".repeat(600)),
            ("b.txt", "x\n".into()),
        ]);
        let root = dir.path();
        let searcher = ContentSearcher::new();
        let full = searcher.search(root, &options("needle"), &go()).unwrap();
        assert!(full.truncated);
        std::fs::remove_file(root.join("many.txt")).unwrap();
        assert!(
            searcher
                .search(root, &options("needle"), &go())
                .unwrap()
                .truncated,
            "the exact query is still served from cache"
        );
        assert!(searcher
            .search(root, &options("needl"), &go())
            .unwrap()
            .matches
            .is_empty());
        assert!(
            searcher
                .search(root, &options("needle\n"), &go())
                .unwrap()
                .truncated
        );
        // A truncated result is not a complete base for a longer query.
        let dir = repo(&[("many.txt", "needle\n".repeat(600))]);
        let root = dir.path();
        searcher.search(root, &options("needle"), &go()).unwrap();
        std::fs::remove_file(root.join("many.txt")).unwrap();
        assert!(searcher
            .search(root, &options("needle2"), &go())
            .unwrap()
            .matches
            .is_empty());

        // Expired entries are not reused.
        let dir = repo(&[("a.txt", "hello\n".into())]);
        let root = dir.path();
        let short = ContentSearcher::with_limits(16, Duration::ZERO);
        assert_eq!(
            short
                .search(root, &options("hello"), &go())
                .unwrap()
                .matches
                .len(),
            1
        );
        std::fs::remove_file(root.join("a.txt")).unwrap();
        assert!(short
            .search(root, &options("hello"), &go())
            .unwrap()
            .matches
            .is_empty());

        // Cancelled searches are not cached.
        let dir = repo(&[("a.txt", "hello\n".into())]);
        let root = dir.path();
        let searcher = ContentSearcher::new();
        assert!(searcher
            .search(root, &options("hello"), &AtomicBool::new(true))
            .unwrap()
            .matches
            .is_empty());
        assert_eq!(
            searcher
                .search(root, &options("hello"), &go())
                .unwrap()
                .matches
                .len(),
            1
        );

        // LRU capacity.
        let small = ContentSearcher::with_limits(2, CONTENT_TTL);
        let dir = repo(&[("a.txt", "one two three\n".into())]);
        let root = dir.path();
        for q in ["one", "two", "three"] {
            assert_eq!(
                small
                    .search(root, &options(q), &go())
                    .unwrap()
                    .matches
                    .len(),
                1
            );
        }
        std::fs::remove_file(root.join("a.txt")).unwrap();
        assert!(small
            .search(root, &options("one"), &go())
            .unwrap()
            .matches
            .is_empty());
        assert_eq!(
            small
                .search(root, &options("three"), &go())
                .unwrap()
                .matches
                .len(),
            1
        );
    }

    #[test]
    fn refining_clipped_previews_recomputes_columns_in_the_full_line() {
        let long = format!("{}needle zz{}\n", "a".repeat(1000), "b".repeat(1000));
        let head = format!("needle zz {}\n", "c".repeat(1000));
        let dir = repo(&[("long.txt", long), ("head.txt", head)]);
        let root = dir.path();
        let searcher = ContentSearcher::new();
        let mut first = lines(&searcher.search(root, &options("needle"), &go()).unwrap());
        first.sort();
        assert_eq!(
            first,
            [("head.txt".into(), 1, 1), ("long.txt".into(), 1, 1001)]
        );
        std::fs::remove_file(root.join("long.txt")).unwrap();
        std::fs::remove_file(root.join("head.txt")).unwrap();
        let mut refined = lines(&searcher.search(root, &options("dle z"), &go()).unwrap());
        assert!(refined.is_empty(), "not an extension");
        refined = lines(&searcher.search(root, &options("needle z"), &go()).unwrap());
        refined.sort();
        assert_eq!(
            refined,
            [("head.txt".into(), 1, 1), ("long.txt".into(), 1, 1001)]
        );
        let refined = searcher
            .search(root, &options("needle zz b"), &go())
            .unwrap();
        assert!(refined.matches.is_empty(), "no b right after the match");
        let refined = searcher
            .search(root, &options("needle zz c"), &go())
            .unwrap();
        assert_eq!(lines(&refined), [("head.txt".into(), 1, 1)]);
        // Columns follow the refined query when it starts earlier.
        let dir = repo(&[(
            "x.txt",
            format!("{}xneedle{}\n", "a".repeat(500), "b".repeat(500)),
        )]);
        let root = dir.path();
        searcher.search(root, &options("needle"), &go()).unwrap();
        std::fs::remove_file(root.join("x.txt")).unwrap();
        let refined = searcher.search(root, &options("axneedle"), &go()).unwrap();
        assert_eq!(lines(&refined), [("x.txt".into(), 1, 500)]);
    }

    #[test]
    fn shared_searcher_is_a_singleton() {
        assert!(std::ptr::eq(content_searcher(), content_searcher()));
        let _ = ContentSearcher::default();
    }
}
