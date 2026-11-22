use super::{command, fuzzy};
use merry_runtime::SkillMetadata;
use std::{
    cmp::Reverse,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const MAX_COMPLETION_ITEMS: usize = 32;
const MAX_WORKSPACE_SCAN_ENTRIES: usize = 20_000;
const MAX_DIRECT_DIR_SCAN_ENTRIES: usize = 4_000;
const WORKSPACE_PATH_CACHE_TTL: Duration = Duration::from_secs(3);

/// Heavy generated or dependency directories skipped during `@` file indexing.
/// Applied to directories only so files such as `build.rs` remain searchable.
const IGNORED_WORKSPACE_DIRS: &[&str] = &[
    "target",
    "node_modules",
    "__pycache__",
    "dist",
    "build",
    "out",
    "vendor",
    "venv",
    "coverage",
    "pkg",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompletionKind {
    Path,
    Skill,
    Slash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletionItem {
    kind: CompletionKind,
    value: String,
    suffix: Option<String>,
    detail: Option<String>,
    append_space: bool,
    match_positions: Vec<usize>,
}

impl CompletionItem {
    fn path(value: String, match_positions: Vec<usize>) -> Self {
        let append_space = !value.ends_with('/');
        Self {
            value,
            suffix: None,
            detail: None,
            kind: CompletionKind::Path,
            append_space,
            match_positions,
        }
    }

    fn skill(value: String, detail: String, match_positions: Vec<usize>) -> Self {
        Self {
            value,
            suffix: None,
            detail: Some(detail),
            kind: CompletionKind::Skill,
            append_space: true,
            match_positions,
        }
    }

    fn slash(
        value: String,
        suffix: Option<String>,
        detail: String,
        append_space: bool,
        match_positions: Vec<usize>,
    ) -> Self {
        Self {
            value,
            suffix,
            detail: Some(detail),
            kind: CompletionKind::Slash,
            append_space,
            match_positions,
        }
    }

    pub(crate) fn kind(&self) -> &CompletionKind {
        &self.kind
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }

    pub(crate) fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    pub(crate) fn match_positions(&self) -> &[usize] {
        &self.match_positions
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompletionMenu {
    trigger: char,
    token_start: usize,
    token_end: usize,
    query: String,
    items: Vec<CompletionItem>,
    selected: usize,
}

impl CompletionMenu {
    pub(crate) fn items(&self) -> &[CompletionItem] {
        &self.items
    }

    pub(crate) fn selected_index(&self) -> usize {
        self.selected
    }

    pub(crate) fn selected_item(&self) -> Option<&CompletionItem> {
        self.items.get(self.selected)
    }

    pub(crate) fn visible_window(&self, max_rows: usize) -> (usize, usize) {
        if self.items.is_empty() || max_rows == 0 {
            return (0, 0);
        }
        let window = max_rows.min(self.items.len());
        let start = if self.selected >= window {
            (self.selected + 1 - window).min(self.items.len() - window)
        } else {
            0
        };
        (start, (start + window).min(self.items.len()))
    }

    pub(crate) fn select_next(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    pub(crate) fn select_previous(&mut self) {
        if !self.items.is_empty() {
            self.selected = self
                .selected
                .checked_sub(1)
                .unwrap_or_else(|| self.items.len().saturating_sub(1));
        }
    }

    pub(crate) fn select_index(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        self.selected = index;
        true
    }

    pub(crate) fn replacement_range(&self) -> std::ops::Range<usize> {
        self.token_start..self.token_end
    }

    pub(crate) fn replacement_text(&self) -> Option<String> {
        let item = self.selected_item()?;
        Some(match item.kind() {
            CompletionKind::Path if item.append_space => {
                format!("@{} ", escape_path_spaces(item.value()))
            }
            CompletionKind::Path => format!("@{}", escape_path_spaces(item.value())),
            CompletionKind::Skill => format!("${} ", item.value()),
            CompletionKind::Slash if item.append_space => format!("{} ", item.value()),
            CompletionKind::Slash => item.value().to_owned(),
        })
    }

    pub(crate) fn is_slash(&self) -> bool {
        self.trigger == '/'
    }

    fn matches(&self, token_start: usize, token_end: usize, query: &str) -> bool {
        self.token_start == token_start && self.token_end == token_end && self.query == query
    }
}

#[derive(Debug, Clone)]
struct WorkspacePathCache {
    include_hidden: bool,
    scanned_at: Instant,
    paths: Arc<[String]>,
}

#[derive(Debug, Clone)]
pub(crate) struct CompletionSources {
    workspace_root: PathBuf,
    skills: Vec<SkillMetadata>,
    path_cache: Arc<Mutex<Option<WorkspacePathCache>>>,
}

impl PartialEq for CompletionSources {
    fn eq(&self, other: &Self) -> bool {
        self.workspace_root == other.workspace_root && self.skills == other.skills
    }
}

impl Eq for CompletionSources {}

impl CompletionSources {
    pub(crate) fn new(workspace_root: PathBuf, mut skills: Vec<SkillMetadata>) -> Self {
        skills.sort_by(|left, right| left.name().cmp(right.name()));
        Self {
            workspace_root,
            skills,
            path_cache: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn skills(&self) -> &[SkillMetadata] {
        &self.skills
    }

    #[cfg(test)]
    pub(crate) fn from_skill_names(workspace_root: PathBuf, skill_names: &[&str]) -> Self {
        let skills = skill_names
            .iter()
            .map(|name| {
                SkillMetadata::new(
                    *name,
                    "test skill",
                    PathBuf::from(format!("skills/{name}/SKILL.md")),
                    PathBuf::from("/skills"),
                )
                .expect("valid test skill")
            })
            .collect::<Vec<_>>();
        Self::new(workspace_root, skills)
    }

    pub(crate) fn menu_for_input(
        &self,
        text: &str,
        cursor: usize,
        previous: Option<&CompletionMenu>,
    ) -> Option<CompletionMenu> {
        let token = active_completion_token(text, cursor)?;
        if let Some(previous) = previous
            && previous.trigger == token.trigger
            && previous.matches(token.start, token.end, token.query)
        {
            return Some(previous.clone());
        }

        let items = match token.trigger {
            '@' => self.path_items(token.query),
            '$' => self.skill_items(token.query),
            '/' => self.slash_items(token.query),
            _ => Vec::new(),
        };
        if items.is_empty() {
            return None;
        }

        let selected = if token.trigger == '/' {
            let full_token = &text[token.start..token.end];
            let canonical_exact = full_token
                .strip_prefix('/')
                .and_then(command::find_slash_exact)
                .and_then(command::CommandSpec::slash_name)
                .map(|name| format!("/{name}"));
            canonical_exact
                .and_then(|target| items.iter().position(|item| item.value() == target))
                .unwrap_or(0)
        } else {
            0
        };
        Some(CompletionMenu {
            trigger: token.trigger,
            token_start: token.start,
            token_end: token.end,
            query: token.query.to_owned(),
            items,
            selected,
        })
    }

    fn workspace_paths(&self, include_hidden: bool) -> Arc<[String]> {
        if let Ok(guard) = self.path_cache.lock()
            && let Some(cache) = guard.as_ref()
            && cache.include_hidden == include_hidden
            && cache.scanned_at.elapsed() < WORKSPACE_PATH_CACHE_TTL
        {
            return Arc::clone(&cache.paths);
        }

        let mut paths = Vec::new();
        collect_workspace_paths(
            &self.workspace_root,
            Path::new(""),
            include_hidden,
            &mut paths,
            &mut 0,
        );
        let shared: Arc<[String]> = paths.into();
        if let Ok(mut guard) = self.path_cache.lock() {
            *guard = Some(WorkspacePathCache {
                include_hidden,
                scanned_at: Instant::now(),
                paths: Arc::clone(&shared),
            });
        }
        shared
    }

    fn path_items(&self, query: &str) -> Vec<CompletionItem> {
        let unescaped = unescape_path_spaces(query);
        let query = unescaped.as_str();
        if let Some(target) = self.parse_direct_path_query(query) {
            return direct_path_items(target);
        }
        let include_hidden = query.starts_with('.') || query.contains("/.");
        let candidates = self.workspace_paths(include_hidden);
        let mut scored = candidates
            .iter()
            .filter_map(|candidate| {
                let matched = fuzzy::fuzzy_match_path(candidate, query)?;
                Some((matched.score, candidate.as_str(), matched.positions))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
        scored.truncate(MAX_COMPLETION_ITEMS);
        scored
            .into_iter()
            .map(|(_, path, positions)| CompletionItem::path(path.to_owned(), positions))
            .collect()
    }

    fn parse_direct_path_query(&self, query: &str) -> Option<DirectPathTarget> {
        let normalized = query.replace('\\', "/");
        if normalized.starts_with('/') {
            let last_slash = normalized.rfind('/')?;
            let prefix = &normalized[..=last_slash];
            let segment = &normalized[last_slash + 1..];
            return Some(DirectPathTarget {
                dir: PathBuf::from(prefix),
                display_prefix: prefix.to_owned(),
                segment_query: segment.to_owned(),
            });
        }
        if normalized == "~" || normalized.starts_with("~/") {
            let home = user_home_dir()?;
            let rest = normalized.strip_prefix("~/").unwrap_or("");
            if let Some(last_slash) = rest.rfind('/') {
                let rel_parent = &rest[..last_slash];
                let segment = &rest[last_slash + 1..];
                return Some(DirectPathTarget {
                    dir: home.join(rel_parent),
                    display_prefix: format!("~/{rel_parent}/"),
                    segment_query: segment.to_owned(),
                });
            }
            return Some(DirectPathTarget {
                dir: home,
                display_prefix: "~/".to_owned(),
                segment_query: rest.to_owned(),
            });
        }
        if is_windows_drive_absolute(&normalized) {
            let last_slash = normalized.rfind('/')?;
            let prefix = &normalized[..=last_slash];
            let segment = &normalized[last_slash + 1..];
            return Some(DirectPathTarget {
                dir: PathBuf::from(prefix),
                display_prefix: prefix.to_owned(),
                segment_query: segment.to_owned(),
            });
        }
        if normalized.starts_with("./") || normalized.starts_with("../") {
            let last_slash = normalized.rfind('/')?;
            let prefix = &normalized[..=last_slash];
            let segment = &normalized[last_slash + 1..];
            return Some(DirectPathTarget {
                dir: self.workspace_root.join(prefix),
                display_prefix: prefix.to_owned(),
                segment_query: segment.to_owned(),
            });
        }
        None
    }

    fn skill_items(&self, query: &str) -> Vec<CompletionItem> {
        if query.is_empty() {
            return self
                .skills
                .iter()
                .take(MAX_COMPLETION_ITEMS)
                .map(|skill| {
                    CompletionItem::skill(
                        skill.name().to_owned(),
                        skill.description().to_owned(),
                        Vec::new(),
                    )
                })
                .collect();
        }

        let mut scored = self
            .skills
            .iter()
            .filter_map(|skill| {
                if let Some(name_match) = fuzzy::fuzzy_match(skill.name(), query) {
                    return Some((0_u8, Reverse(name_match.score), skill, name_match.positions));
                }
                let desc_match = fuzzy::fuzzy_match(skill.description(), query)?;
                Some((1_u8, Reverse(desc_match.score), skill, Vec::new()))
            })
            .collect::<Vec<_>>();

        scored.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.name().cmp(right.2.name()))
        });
        scored.truncate(MAX_COMPLETION_ITEMS);
        scored
            .into_iter()
            .map(|(_, _, skill, positions)| {
                CompletionItem::skill(
                    skill.name().to_owned(),
                    skill.description().to_owned(),
                    positions,
                )
            })
            .collect()
    }

    fn slash_items(&self, query: &str) -> Vec<CompletionItem> {
        command::find_slash_matches(query)
            .into_iter()
            .filter_map(|command| {
                let name = command.slash_name()?;
                let description = command.slash_description()?;
                let suffix = command.arg_hint().map(|hint| format!(" {hint}"));
                let detail = if command.slash_aliases().is_empty() {
                    description.to_owned()
                } else {
                    let aliases = command
                        .slash_aliases()
                        .iter()
                        .map(|alias| format!("/{alias}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("{description} ({aliases})")
                };
                let match_positions = if query.is_empty() {
                    Vec::new()
                } else {
                    fuzzy::fuzzy_match(name, query)
                        .map(|matched| matched.positions.into_iter().map(|pos| pos + 1).collect())
                        .unwrap_or_default()
                };
                Some(CompletionItem::slash(
                    format!("/{name}"),
                    suffix,
                    detail,
                    command.accepts_args(),
                    match_positions,
                ))
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompletionToken<'a> {
    trigger: char,
    start: usize,
    end: usize,
    query: &'a str,
}

fn escape_path_spaces(path: &str) -> String {
    if !path.contains(' ') {
        return path.to_owned();
    }
    path.replace(' ', "\\ ")
}

fn unescape_path_spaces(query: &str) -> String {
    if !query.contains("\\ ") {
        return query.to_owned();
    }
    query.replace("\\ ", " ")
}

fn is_escaped_space(text: &str, space_byte_index: usize) -> bool {
    let backslash_count = text[..space_byte_index]
        .bytes()
        .rev()
        .take_while(|&byte| byte == b'\\')
        .count();
    backslash_count % 2 == 1
}

fn active_completion_token(text: &str, cursor: usize) -> Option<CompletionToken<'_>> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }

    let mut start = cursor;
    for (index, value) in text[..cursor].char_indices().rev() {
        if value.is_whitespace() && !(value == ' ' && is_escaped_space(text, index)) {
            break;
        }
        start = index;
    }

    let mut end = text.len();
    for (offset, value) in text[cursor..].char_indices() {
        let abs_index = cursor + offset;
        if value.is_whitespace() && !(value == ' ' && is_escaped_space(text, abs_index)) {
            end = abs_index;
            break;
        }
    }
    let token = &text[start..end];
    let mut chars = token.char_indices();
    let (_, trigger) = chars.next()?;
    if !matches!(trigger, '@' | '$' | '/') {
        return None;
    }
    let query_start = start + trigger.len_utf8();
    if cursor < query_start {
        return None;
    }
    if trigger == '/' && (start != 0 || text.contains(['\n', '\r'])) {
        return None;
    }
    if token[trigger.len_utf8()..]
        .chars()
        .any(|value| match trigger {
            '@' => matches!(value, '@' | '$'),
            '$' | '/' => matches!(value, '@' | '$' | '/'),
            _ => false,
        })
    {
        return None;
    }

    Some(CompletionToken {
        trigger,
        start,
        end,
        query: &text[query_start..cursor],
    })
}

fn is_ignored_dir(name: &str, include_hidden: bool) -> bool {
    if matches!(
        name,
        ".git" | ".hg" | ".svn" | ".venv" | ".next" | ".nuxt" | ".turbo" | ".cache"
    ) {
        return true;
    }
    if !include_hidden && name.starts_with('.') {
        return true;
    }
    IGNORED_WORKSPACE_DIRS.contains(&name)
}

#[derive(Debug, Clone)]
struct DirectPathTarget {
    dir: PathBuf,
    display_prefix: String,
    segment_query: String,
}

fn is_windows_drive_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn direct_path_items(target: DirectPathTarget) -> Vec<CompletionItem> {
    let Ok(entries) = fs::read_dir(&target.dir) else {
        return Vec::new();
    };
    let include_hidden = target.segment_query.starts_with('.');
    let prefix_char_offset = target.display_prefix.chars().count();
    let mut scored = Vec::new();

    for entry in entries
        .filter_map(Result::ok)
        .take(MAX_DIRECT_DIR_SCAN_ENTRIES)
    {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if !include_hidden && name.starts_with('.') {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_dir = file_type.is_dir()
            || (file_type.is_symlink() && fs::metadata(entry.path()).is_ok_and(|m| m.is_dir()));
        let mut segment = name.into_owned();
        if is_dir {
            segment.push('/');
        }
        let Some(matched) = fuzzy::fuzzy_match(&segment, &target.segment_query) else {
            continue;
        };
        let display = format!("{}{segment}", target.display_prefix);
        let positions = matched
            .positions
            .into_iter()
            .map(|pos| pos + prefix_char_offset)
            .collect::<Vec<_>>();
        scored.push((is_dir, matched.score, display, positions));
    }

    if target.segment_query.is_empty() {
        scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.2.cmp(&right.2)));
    } else {
        scored.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.2.cmp(&right.2)));
    }
    scored.truncate(MAX_COMPLETION_ITEMS);
    scored
        .into_iter()
        .map(|(_, _, display, positions)| CompletionItem::path(display, positions))
        .collect()
}

fn collect_workspace_paths(
    root: &Path,
    relative_dir: &Path,
    include_hidden: bool,
    items: &mut Vec<String>,
    scanned: &mut usize,
) {
    if *scanned >= MAX_WORKSPACE_SCAN_ENTRIES {
        return;
    }

    let dir = root.join(relative_dir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        if *scanned >= MAX_WORKSPACE_SCAN_ENTRIES {
            return;
        }
        *scanned += 1;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if is_ignored_dir(&name, include_hidden) {
                continue;
            }
        } else if !include_hidden && name.starts_with('.') {
            continue;
        }

        let relative = relative_dir.join(name.as_ref());
        let mut candidate = relative.to_string_lossy().replace('\\', "/");
        if file_type.is_dir() {
            candidate.push('/');
        }
        items.push(candidate);
        if file_type.is_dir() {
            collect_workspace_paths(root, &relative, include_hidden, items, scanned);
        }
    }
}
