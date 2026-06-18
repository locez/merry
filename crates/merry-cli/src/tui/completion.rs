use merry_runtime::SkillMetadata;
use std::{
    fs,
    path::{Path, PathBuf},
};

const MAX_COMPLETION_ITEMS: usize = 8;
const MAX_WORKSPACE_SCAN_ENTRIES: usize = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum CompletionKind {
    Path,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct CompletionItem {
    kind: CompletionKind,
    value: String,
    detail: Option<String>,
}

impl CompletionItem {
    fn path(value: String) -> Self {
        Self {
            value,
            detail: None,
            kind: CompletionKind::Path,
        }
    }

    fn skill(value: String, detail: String) -> Self {
        Self {
            value,
            detail: Some(detail),
            kind: CompletionKind::Skill,
        }
    }

    pub(crate) fn kind(&self) -> &CompletionKind {
        &self.kind
    }

    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
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

    pub(crate) fn replacement_range(&self) -> std::ops::Range<usize> {
        self.token_start..self.token_end
    }

    pub(crate) fn replacement_text(&self) -> Option<String> {
        let item = self.selected_item()?;
        Some(match item.kind() {
            CompletionKind::Path => format!("@{} ", item.value()),
            CompletionKind::Skill => format!("${} ", item.value()),
        })
    }

    fn matches(&self, token_start: usize, token_end: usize, query: &str) -> bool {
        self.token_start == token_start && self.token_end == token_end && self.query == query
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) struct CompletionSources {
    workspace_root: PathBuf,
    skills: Vec<SkillCompletion>,
}

impl CompletionSources {
    pub(crate) fn new(workspace_root: PathBuf, skills: Vec<SkillMetadata>) -> Self {
        let mut skills = skills
            .into_iter()
            .map(|skill| SkillCompletion {
                name: skill.name().to_owned(),
                description: skill.description().to_owned(),
            })
            .collect::<Vec<_>>();
        skills.sort_by(|left, right| left.name.cmp(&right.name));
        Self {
            workspace_root,
            skills,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_skill_names(workspace_root: PathBuf, skill_names: &[&str]) -> Self {
        let mut skills = skill_names
            .iter()
            .map(|name| SkillCompletion {
                name: (*name).to_owned(),
                description: String::new(),
            })
            .collect::<Vec<_>>();
        skills.sort_by(|left, right| left.name.cmp(&right.name));
        Self {
            workspace_root,
            skills,
        }
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
            _ => Vec::new(),
        };
        if items.is_empty() {
            return None;
        }

        Some(CompletionMenu {
            trigger: token.trigger,
            token_start: token.start,
            token_end: token.end,
            query: token.query.to_owned(),
            items,
            selected: 0,
        })
    }

    fn path_items(&self, query: &str) -> Vec<CompletionItem> {
        let mut items = Vec::new();
        collect_path_items(
            &self.workspace_root,
            Path::new(""),
            query,
            &mut items,
            &mut 0,
        );
        items.sort();
        items.truncate(MAX_COMPLETION_ITEMS);
        items.into_iter().map(CompletionItem::path).collect()
    }

    fn skill_items(&self, query: &str) -> Vec<CompletionItem> {
        self.skills
            .iter()
            .filter(|skill| skill.name.starts_with(query))
            .take(MAX_COMPLETION_ITEMS)
            .map(|skill| CompletionItem::skill(skill.name.clone(), skill.description.clone()))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillCompletion {
    name: String,
    description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompletionToken<'a> {
    trigger: char,
    start: usize,
    end: usize,
    query: &'a str,
}

fn active_completion_token(text: &str, cursor: usize) -> Option<CompletionToken<'_>> {
    if cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }

    let mut start = cursor;
    for (index, value) in text[..cursor].char_indices().rev() {
        if value.is_whitespace() {
            break;
        }
        start = index;
    }

    let token = &text[start..cursor];
    let mut chars = token.char_indices();
    let (_, trigger) = chars.next()?;
    if !matches!(trigger, '@' | '$') {
        return None;
    }
    if token[trigger.len_utf8()..]
        .chars()
        .any(|value| matches!(value, '@' | '$'))
    {
        return None;
    }

    Some(CompletionToken {
        trigger,
        start,
        end: cursor,
        query: &token[trigger.len_utf8()..],
    })
}

fn collect_path_items(
    root: &Path,
    relative_dir: &Path,
    query: &str,
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
        if name.starts_with('.') {
            continue;
        }
        let relative = relative_dir.join(name.as_ref());
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let mut candidate = relative.to_string_lossy().replace('\\', "/");
        if file_type.is_dir() {
            candidate.push('/');
        }
        if fuzzy_path_match(&candidate, query) {
            items.push(candidate);
        }
        if file_type.is_dir() {
            collect_path_items(root, &relative, query, items, scanned);
        }
    }
}

fn fuzzy_path_match(candidate: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let mut query_chars = query.chars().flat_map(char::to_lowercase);
    let Some(mut expected) = query_chars.next() else {
        return true;
    };
    for value in candidate.chars().flat_map(char::to_lowercase) {
        if value == expected {
            match query_chars.next() {
                Some(next) => expected = next,
                None => return true,
            }
        }
    }
    false
}
