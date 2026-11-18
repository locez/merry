//! One-shot consolidation of claimed inbox notes into workspace topics.
//!
//! Batch dream is a separate runner and is not implemented. This pass reads
//! the claimed notes and the current topic files, asks for one JSON plan, and
//! applies that plan only when every operation cites those notes.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::Deserialize;

use super::{
    DurableMemoryError,
    capture::strip_json_fence,
    manifest::truncate_utf8,
    paths::{ARCHIVE_DIR, INBOX_DIR, TOPICS_DIR},
    policy::{OBSERVATION_MAX_BYTES, TOPIC_MAX_BYTES},
    state::atomic_write,
};

pub(crate) const MIN_PENDING_COUNT: usize = 20;
pub(crate) const MAX_PENDING_AGE: Duration = Duration::from_secs(24 * 60 * 60);
pub(crate) const CLAIM_LIMIT: usize = 20;
const PROMPT_FILE_BYTES: usize = 16 * 1024;
const PROMPT_TOPIC_BUDGET: usize = 64 * 1024;
const PROVENANCE_MARKER: &str = "<!-- memory-v2 provenance -->";

pub(crate) const DREAM_SYSTEM_PROMPT: &str = r##"Consolidate only the supplied claimed observations into the supplied curated topics. Topics are reference notes for a future agent that has not seen any conversation and will read them before starting related work. A topic covers one broad subject area (a system, a repository area, a tool, a person, or a workflow); its sub-areas are `##` sections, not separate topics. A workspace usually needs fewer than ten topics. Treat `topic_hint` as a section suggestion; add to the existing topic whose area covers an observation rather than creating a new one, and split only above about 16 KB. Each topic starts with a `# Title` and one sentence stating what it covers (an index shows only those two lines), then durable facts as short standalone statements under `##` headings: file paths, constants, commands, invariants, decisions, and user preferences. Put changing state under `## Recent state` or omit it. Do not narrate conversations. Drop facts that newer observations contradict or make stale. Return one JSON object with an `operations` array. Every operation must have an `op` field identifying its type: create, update, delete, rename, merge, or split. Use exactly the fields listed for each type; do not add other fields. create and update: op, path, content, evidence. delete: op, path, evidence. rename: op, from, to, content, evidence. merge: op, sources, destination, content, evidence. split: op, source, destinations, evidence. evidence and sources are arrays of path strings. destinations is an array of objects containing exactly path and content. All other fields are strings. Example: {"operations":[{"op":"create","path":"topics/preferences.md","content":"# Reply preferences\nHow the user wants answers written.\n\n## Style\n- Prefer concise answers.","evidence":["observations/_inbox/example.md"]}]}. The example is illustrative; use only facts and exact evidence paths supplied in the input. Every operation must cite one or more exact claimed observation paths in `evidence`. Every topic path must be exactly `topics/<slug>.md`: one markdown file directly in `topics/`, no subdirectories. A slug is lowercase letters, digits, and hyphens, for example `topics/preferences.md`. Preserve still-valid facts. Do not include `<!-- memory-v2 provenance -->` comments in content; they are stripped and not stored. Do not mention or attempt tools, shell, workspace files, MEMORY.md, databases, archive, extraction, or another Dream."##;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimedNote {
    pub(crate) relative_path: String,
    pub(crate) absolute_path: PathBuf,
    pub(crate) modified: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InboxScan {
    pub(crate) eligible: Vec<ClaimedNote>,
    pub(crate) oversized: usize,
}

impl InboxScan {
    #[must_use]
    pub(crate) fn oldest_age(&self, now: SystemTime) -> Option<Duration> {
        self.eligible
            .iter()
            .filter_map(|note| now.duration_since(note.modified).ok())
            .max()
    }
}

pub(crate) fn dream_should_run(scan: &InboxScan, now: SystemTime) -> bool {
    if scan.eligible.is_empty() {
        return false;
    }
    scan.eligible.len() >= MIN_PENDING_COUNT
        || scan
            .oldest_age(now)
            .is_some_and(|age| age >= MAX_PENDING_AGE)
}

pub(crate) fn scan_inbox(scope_dir: &Path) -> Result<InboxScan, DurableMemoryError> {
    let directory = scope_dir.join(INBOX_DIR);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InboxScan {
                eligible: Vec::new(),
                oversized: 0,
            });
        }
        Err(source) => {
            return Err(DurableMemoryError::Io {
                operation: "list memory inbox",
                path: directory,
                source,
            });
        }
    };
    let mut eligible = Vec::new();
    let mut oversized = 0usize;
    for entry in entries {
        let entry = entry.map_err(|source| DurableMemoryError::Io {
            operation: "read memory inbox entry",
            path: directory.clone(),
            source,
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.ends_with(".md") || name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| DurableMemoryError::Io {
            operation: "inspect memory observation",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        if metadata.len() > OBSERVATION_MAX_BYTES as u64 {
            oversized += 1;
            tracing::warn!(
                path = %path.display(),
                bytes = metadata.len(),
                "oversized memory observation is excluded from dream"
            );
            continue;
        }
        eligible.push(ClaimedNote {
            relative_path: format!("{INBOX_DIR}/{name}"),
            absolute_path: path,
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }
    eligible.sort_by(|left, right| {
        left.modified
            .cmp(&right.modified)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    Ok(InboxScan {
        eligible,
        oversized,
    })
}

pub(crate) fn render_dream_input(
    scope_dir: &Path,
    claimed: &[ClaimedNote],
) -> Result<String, DurableMemoryError> {
    let mut input = String::from("Claimed observations:\n");
    for note in claimed {
        let bytes = fs::read(&note.absolute_path).map_err(|source| DurableMemoryError::Io {
            operation: "read claimed observation",
            path: note.absolute_path.clone(),
            source,
        })?;
        let text = String::from_utf8_lossy(&bytes);
        input.push_str("\n--- ");
        input.push_str(&note.relative_path);
        input.push('\n');
        input.push_str(truncate_utf8(&text, PROMPT_FILE_BYTES));
        if !input.ends_with('\n') {
            input.push('\n');
        }
    }
    input.push_str("\nExisting topics:\n");
    let mut remaining = PROMPT_TOPIC_BUDGET;
    let mut omitted = 0usize;
    for (relative, absolute) in list_topic_files(scope_dir)? {
        let bytes = fs::read(&absolute).map_err(|source| DurableMemoryError::Io {
            operation: "read memory topic",
            path: absolute,
            source,
        })?;
        let text = String::from_utf8_lossy(&bytes);
        let excerpt = truncate_utf8(&text, PROMPT_FILE_BYTES.min(remaining));
        if excerpt.is_empty() {
            omitted += 1;
            continue;
        }
        remaining = remaining.saturating_sub(excerpt.len());
        input.push_str("\n--- ");
        input.push_str(&relative);
        input.push('\n');
        input.push_str(excerpt);
        if !input.ends_with('\n') {
            input.push('\n');
        }
    }
    if omitted > 0 {
        input.push_str(&format!(
            "\n{omitted} additional topics were omitted from this prompt to stay within the budget.\n"
        ));
    }
    Ok(input)
}

fn list_topic_files(scope_dir: &Path) -> Result<Vec<(String, PathBuf)>, DurableMemoryError> {
    let directory = scope_dir.join(TOPICS_DIR);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(DurableMemoryError::Io {
                operation: "list memory topics",
                path: directory,
                source,
            });
        }
    };
    let mut topics = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| DurableMemoryError::Io {
            operation: "read memory topic entry",
            path: directory.clone(),
            source,
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_topic_file_name(&name) {
            continue;
        }
        let path = entry.path();
        let metadata = entry.metadata().map_err(|source| DurableMemoryError::Io {
            operation: "inspect memory topic",
            path: path.clone(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            continue;
        }
        topics.push((format!("{TOPICS_DIR}/{name}"), path));
    }
    topics.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(topics)
}

pub(crate) fn dream_plan_schema() -> serde_json::Value {
    let strings = serde_json::json!({ "type": "array", "items": { "type": "string" } });
    let operation = |op: &str, fields: &[(&str, serde_json::Value)]| {
        let mut properties = serde_json::Map::new();
        properties.insert(
            "op".to_owned(),
            serde_json::json!({ "type": "string", "enum": [op] }),
        );
        let mut required = vec![serde_json::Value::from("op")];
        for (name, schema) in fields {
            properties.insert((*name).to_owned(), schema.clone());
            required.push(serde_json::Value::from(*name));
        }
        properties.insert("evidence".to_owned(), strings.clone());
        required.push(serde_json::Value::from("evidence"));
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "required": required,
            "properties": properties
        })
    };
    let string = serde_json::json!({ "type": "string" });
    let destination = serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path", "content"],
        "properties": { "path": string, "content": string }
    });
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["operations"],
        "properties": {
            "operations": {
                "type": "array",
                "items": {
                    "anyOf": [
                        operation("create", &[("path", string.clone()), ("content", string.clone())]),
                        operation("update", &[("path", string.clone()), ("content", string.clone())]),
                        operation("delete", &[("path", string.clone())]),
                        operation("rename", &[("from", string.clone()), ("to", string.clone()), ("content", string.clone())]),
                        operation("merge", &[("sources", strings.clone()), ("destination", string.clone()), ("content", string.clone())]),
                        operation("split", &[("source", string.clone()), ("destinations", serde_json::json!({ "type": "array", "items": destination }))])
                    ]
                }
            }
        }
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DreamPlan {
    operations: Vec<DreamOperation>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum DreamOperation {
    Create {
        path: String,
        content: String,
        evidence: Vec<String>,
    },
    Update {
        path: String,
        content: String,
        evidence: Vec<String>,
    },
    Delete {
        path: String,
        evidence: Vec<String>,
    },
    Rename {
        from: String,
        to: String,
        content: String,
        evidence: Vec<String>,
    },
    Merge {
        sources: Vec<String>,
        destination: String,
        content: String,
        evidence: Vec<String>,
    },
    Split {
        source: String,
        destinations: Vec<DreamDestination>,
        evidence: Vec<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DreamDestination {
    path: String,
    content: String,
}

pub(crate) fn apply_plan(
    scope_dir: &Path,
    claimed: &[ClaimedNote],
    response: &str,
) -> Result<usize, DurableMemoryError> {
    let response = strip_json_fence(response);
    let plan: DreamPlan =
        serde_json::from_str(response).map_err(|error| DurableMemoryError::Denied {
            message: format!("invalid Dream plan: {error}"),
        })?;
    if plan.operations.is_empty() {
        return Err(DurableMemoryError::Denied {
            message: "invalid Dream plan: operations must not be empty".to_owned(),
        });
    }
    let claimed_paths = claimed
        .iter()
        .map(|note| note.relative_path.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    for operation in &plan.operations {
        validate_operation(scope_dir, operation, &claimed_paths)?;
    }
    for operation in &plan.operations {
        apply_operation(scope_dir, operation)?;
    }
    for note in claimed {
        archive_note(scope_dir, note)?;
    }
    Ok(plan.operations.len())
}

fn validate_operation(
    scope_dir: &Path,
    operation: &DreamOperation,
    claimed: &std::collections::BTreeSet<&str>,
) -> Result<(), DurableMemoryError> {
    match operation {
        DreamOperation::Create {
            path,
            content,
            evidence,
        } => {
            require_evidence(evidence, claimed)?;
            require_new_topic(scope_dir, path)?;
            prepare_content(content)?;
        }
        DreamOperation::Update {
            path,
            content,
            evidence,
        } => {
            require_evidence(evidence, claimed)?;
            require_existing_topic(scope_dir, path)?;
            prepare_content(content)?;
        }
        DreamOperation::Delete { path, evidence } => {
            require_evidence(evidence, claimed)?;
            require_existing_topic(scope_dir, path)?;
        }
        DreamOperation::Rename {
            from,
            to,
            content,
            evidence,
        } => {
            require_evidence(evidence, claimed)?;
            require_existing_topic(scope_dir, from)?;
            require_new_topic(scope_dir, to)?;
            if from == to {
                return Err(invalid("rename source and destination are the same"));
            }
            prepare_content(content)?;
        }
        DreamOperation::Merge {
            sources,
            destination,
            content,
            evidence,
        } => {
            require_evidence(evidence, claimed)?;
            if sources.is_empty() {
                return Err(invalid("merge sources must not be empty"));
            }
            for source in sources {
                require_existing_topic(scope_dir, source)?;
            }
            require_topic_path(destination)?;
            if !sources.iter().any(|source| source == destination) {
                require_new_topic(scope_dir, destination)?;
            }
            prepare_content(content)?;
        }
        DreamOperation::Split {
            source,
            destinations,
            evidence,
        } => {
            require_evidence(evidence, claimed)?;
            require_existing_topic(scope_dir, source)?;
            if destinations.is_empty() {
                return Err(invalid("split destinations must not be empty"));
            }
            for destination in destinations {
                require_topic_path(&destination.path)?;
                if destination.path != *source {
                    require_new_topic(scope_dir, &destination.path)?;
                }
                prepare_content(&destination.content)?;
            }
        }
    }
    Ok(())
}

fn apply_operation(scope_dir: &Path, operation: &DreamOperation) -> Result<(), DurableMemoryError> {
    match operation {
        DreamOperation::Create { path, content, .. }
        | DreamOperation::Update { path, content, .. } => {
            write_topic(scope_dir, path, content)?;
        }
        DreamOperation::Delete { path, .. } => remove_topic(scope_dir, path)?,
        DreamOperation::Rename {
            from, to, content, ..
        } => {
            write_topic(scope_dir, to, content)?;
            remove_topic(scope_dir, from)?;
        }
        DreamOperation::Merge {
            sources,
            destination,
            content,
            ..
        } => {
            write_topic(scope_dir, destination, content)?;
            for source in sources {
                if source != destination {
                    remove_topic(scope_dir, source)?;
                }
            }
        }
        DreamOperation::Split {
            source,
            destinations,
            ..
        } => {
            for destination in destinations {
                write_topic(scope_dir, &destination.path, &destination.content)?;
            }
            if destinations
                .iter()
                .all(|destination| destination.path != *source)
            {
                remove_topic(scope_dir, source)?;
            }
        }
    }
    Ok(())
}

fn require_evidence(
    evidence: &[String],
    claimed: &std::collections::BTreeSet<&str>,
) -> Result<(), DurableMemoryError> {
    if evidence.is_empty() {
        return Err(invalid("every operation must cite evidence"));
    }
    for path in evidence {
        if !claimed.contains(path.as_str()) {
            return Err(invalid(format!(
                "evidence path is not a claimed observation: {path}"
            )));
        }
    }
    Ok(())
}

fn require_topic_path(path: &str) -> Result<(), DurableMemoryError> {
    if is_topic_relative(path) {
        Ok(())
    } else {
        Err(invalid(format!(
            "topic path must be topics/<slug>.md: {path}"
        )))
    }
}

fn require_new_topic(scope_dir: &Path, path: &str) -> Result<(), DurableMemoryError> {
    require_topic_path(path)?;
    if topic_file(scope_dir, path)?.exists() {
        Err(invalid(format!("topic already exists: {path}")))
    } else {
        Ok(())
    }
}

fn require_existing_topic(scope_dir: &Path, path: &str) -> Result<(), DurableMemoryError> {
    require_topic_path(path)?;
    let file = topic_file(scope_dir, path)?;
    if file.is_file() {
        Ok(())
    } else {
        Err(invalid(format!("topic does not exist: {path}")))
    }
}

fn prepare_content(content: &str) -> Result<String, DurableMemoryError> {
    let stripped = content
        .lines()
        .filter(|line| !line.contains(PROVENANCE_MARKER))
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = stripped.trim_start_matches(['\n', '\r']);
    if !trimmed.starts_with("# ") {
        return Err(invalid("topic content must start with a '# ' title"));
    }
    if trimmed.len() > TOPIC_MAX_BYTES {
        return Err(invalid("topic content exceeds 256 KiB"));
    }
    Ok(trimmed.to_owned())
}

fn write_topic(scope_dir: &Path, relative: &str, content: &str) -> Result<(), DurableMemoryError> {
    let content = prepare_content(content)?;
    let path = topic_file(scope_dir, relative)?;
    atomic_write(&path, content.as_bytes())
}

fn remove_topic(scope_dir: &Path, relative: &str) -> Result<(), DurableMemoryError> {
    let path = topic_file(scope_dir, relative)?;
    if path.exists() {
        fs::remove_file(&path).map_err(|source| DurableMemoryError::Io {
            operation: "remove memory topic",
            path,
            source,
        })?;
    }
    Ok(())
}

fn archive_note(scope_dir: &Path, note: &ClaimedNote) -> Result<(), DurableMemoryError> {
    let name = note
        .absolute_path
        .file_name()
        .ok_or_else(|| invalid("claimed observation has no file name"))?;
    let archive = scope_dir.join(ARCHIVE_DIR);
    fs::create_dir_all(&archive).map_err(|source| DurableMemoryError::Io {
        operation: "create memory archive",
        path: archive.clone(),
        source,
    })?;
    let mut destination = archive.join(name);
    if destination.exists() {
        let stem = Path::new(name)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("observation");
        destination = archive.join(format!("{stem}-{}.md", super::state::unix_now()));
    }
    fs::rename(&note.absolute_path, &destination).map_err(|source| DurableMemoryError::Io {
        operation: "archive memory observation",
        path: destination,
        source,
    })
}

fn topic_file(scope_dir: &Path, relative: &str) -> Result<PathBuf, DurableMemoryError> {
    require_topic_path(relative)?;
    Ok(scope_dir.join(relative))
}

fn is_topic_relative(path: &str) -> bool {
    let Some(name) = path.strip_prefix("topics/") else {
        return false;
    };
    is_topic_file_name(name) && !name.contains('/')
}

fn is_topic_file_name(name: &str) -> bool {
    let Some(slug) = name.strip_suffix(".md") else {
        return false;
    };
    let mut parts = slug.split('-');
    parts.all(|part| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

fn invalid(message: impl Into<String>) -> DurableMemoryError {
    DurableMemoryError::Denied {
        message: message.into(),
    }
}
