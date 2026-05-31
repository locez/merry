# Skill Progressive Disclosure Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add filesystem-backed `SKILL.md` discovery to Merry and project skill metadata into the cacheable stable prefix, while keeping full skill bodies available only through `workspace_read_file`.

**Architecture:** `merry-runtime` owns skill metadata types, loading, validation, deterministic rendering, and stable-prefix request assembly. `merry-cli` owns config parsing and wires skill roots only into runtime paths that register `workspace_read_file`. `merry-tool-workspace` remains the only file-read path; no `skill_read` tool or runtime skill selector is introduced.

**Tech Stack:** Rust 2024, `serde`, `toml`, `merry-runtime`, `merry-cli`, `merry-tool-workspace`, deterministic fake providers, `tempfile` for CLI/config tests.

---

## Design Inputs

- Spec: `specs/2026-05-31-skill-progressive-disclosure.md`.
- Local reference: `.merry/codex/codex-rs/core-skills/src/loader.rs`.
- Local reference: `.merry/codex/codex-rs/core-skills/src/render.rs`.
- External reference recorded in the spec: Anthropic Agent Skills progressive disclosure overview.
- Repository rule: when adding accepted config keys, update `examples/config.toml` in the same change.
- User correction: skill metadata belongs in the stable prefix after runtime base instructions and before project rules, so provider KV cache can reuse it.
- User correction: there will not be a no-file-read skill mode; do not add `skill_read(skill_id)`.

## Scope

This plan implements the first runtime-visible skills slice:

- Load `SKILL.md` frontmatter metadata from configured roots.
- Render only `name`, `description`, and workspace-readable `SKILL.md` path in the stable prefix.
- Keep full skill bodies, references, scripts, and assets out of the prefix.
- Use `workspace_read_file` for body reads.
- Wire config into CLI/runtime builder paths used by deterministic and live coding-loop smokes.

This plan does not implement:

- deterministic runtime skill activation;
- explicit `$skill` syntax body injection;
- plugin bundles;
- subagents;
- live model selection-quality smoke.

## File Structure

- Create `crates/merry-runtime/src/skill.rs`: skill metadata types, `SkillCatalog`, directory scan, frontmatter parsing, deterministic rendering, and `SkillLoadWarning`.
- Modify `crates/merry-runtime/src/lib.rs`: export `SkillCatalog`, `SkillMetadata`, `SkillLoadWarning`, and `SkillError` as unstable runtime-facing types.
- Modify `crates/merry-runtime/src/runtime.rs`: add `RuntimeBuilder::skill_catalog`, store the catalog in session state, and pass it to step request compilation.
- Modify `crates/merry-runtime/src/step.rs`: insert the rendered available-skills message into the stable prefix after base instructions and before project rules.
- Modify `crates/merry-runtime/tests/provider_boundary.rs`: prove stable-prefix ordering, hashing, body exclusion, and unchanged dynamic hash.
- Modify `crates/merry-cli/src/config.rs`: parse `[skills] enabled` and `roots`, resolve roots relative to config dir, and validate the example config.
- Modify `crates/merry-cli/src/main.rs`: apply configured skill catalog only to coding-loop runtime builders that register `workspace_read_file`; include skill roots in workspace tool roots for coding-loop profiles.
- Modify `examples/config.toml`: document `[skills]`.
- Modify `crates/merry-runtime/Cargo.toml`: add `tempfile.workspace = true` as a dev dependency for skill loader tests.

## Acceptance Commands

Focused checks after each task:

```bash
cargo test -p merry-runtime skill
cargo test -p merry-runtime compiled_provider_request_skill_metadata
cargo test -p merry-cli config::tests::parses_skill_config_roots
cargo test -p merry-cli coding_loop_runtime_projects_skill_metadata_without_body
cargo test -p merry-cli coding_loop_runtime_includes_skill_roots_in_workspace_read_tools
```

Broader checks before reporting implementation complete:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

No live model test is required for this plan.

## Task 1: Runtime Skill Metadata Types And Renderer

**Files:**
- Create: `crates/merry-runtime/src/skill.rs`
- Modify: `crates/merry-runtime/src/lib.rs`

- [ ] **Step 1: Add failing renderer tests**

Create `crates/merry-runtime/src/skill.rs` with these tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn metadata(name: &str, description: &str, path: &str) -> SkillMetadata {
        SkillMetadata::new(
            name,
            description,
            PathBuf::from(path),
            PathBuf::from("/workspace"),
        )
        .expect("valid skill metadata")
    }

    #[test]
    fn renders_available_skills_without_bodies() {
        let catalog = SkillCatalog::from_metadata(vec![
            metadata(
                "frontend-design",
                "Use for polished frontend implementation.",
                "skills/frontend-design/SKILL.md",
            ),
            metadata(
                "debugging",
                "Use for systematic debugging.",
                "skills/debugging/SKILL.md",
            ),
        ])
        .expect("valid catalog");

        let rendered = catalog
            .to_stable_prefix_message_text()
            .expect("catalog should render");

        assert!(rendered.contains("## Skills"));
        assert!(rendered.contains("frontend-design"));
        assert!(rendered.contains("Use for polished frontend implementation."));
        assert!(rendered.contains("skills/frontend-design/SKILL.md"));
        assert!(rendered.contains("workspace_read_file"));
        assert!(rendered.contains("Read only the referenced files needed"));
        assert!(!rendered.contains("# Frontend Design"));
        assert!(!rendered.contains("full skill body sentinel"));
    }

    #[test]
    fn metadata_order_is_deterministic() {
        let first = SkillCatalog::from_metadata(vec![
            metadata("zeta", "Last alphabetically.", "skills/zeta/SKILL.md"),
            metadata("alpha", "First alphabetically.", "skills/alpha/SKILL.md"),
        ])
        .expect("valid catalog");
        let second = SkillCatalog::from_metadata(vec![
            metadata("alpha", "First alphabetically.", "skills/alpha/SKILL.md"),
            metadata("zeta", "Last alphabetically.", "skills/zeta/SKILL.md"),
        ])
        .expect("valid catalog");

        assert_eq!(
            first.to_stable_prefix_message_text().expect("renders"),
            second.to_stable_prefix_message_text().expect("renders")
        );
    }

    #[test]
    fn rejects_blank_or_control_metadata() {
        let blank = SkillMetadata::new(
            " ",
            "Valid description.",
            PathBuf::from("skills/blank/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect_err("blank name should be rejected");
        assert!(blank.to_string().contains("skill name"));

        let control = SkillMetadata::new(
            "bad\u{7}name",
            "Valid description.",
            PathBuf::from("skills/bad/SKILL.md"),
            PathBuf::from("/workspace"),
        )
        .expect_err("control characters should be rejected");
        assert!(control.to_string().contains("skill name"));
    }
}
```

- [ ] **Step 2: Run tests and confirm failure**

Run:

```bash
cargo test -p merry-runtime skill
```

Expected: FAIL because `skill.rs` is not wired into the crate or the types are missing.

- [ ] **Step 3: Implement minimal metadata and renderer**

Add `mod skill;` to `crates/merry-runtime/src/lib.rs` and export:

```rust
pub use skill::{SkillCatalog, SkillError, SkillLoadWarning, SkillMetadata};
```

Implement `crates/merry-runtime/src/skill.rs`:

```rust
use std::{
    collections::{BTreeMap, btree_map::Entry},
    path::PathBuf,
};
use thiserror::Error;

const SKILLS_INTRO: &str = "A skill is a set of local instructions stored in a `SKILL.md` file. The list below is for discovery only; skill bodies stay on disk until needed.";
const SKILLS_HOW_TO_USE: &str = r#"- If the user explicitly names a skill, use it for that turn.
- If the task clearly matches a skill description, read that skill's `SKILL.md` before relying on it.
- Use `workspace_read_file` to read the listed `SKILL.md`.
- Resolve relative paths mentioned by `SKILL.md` relative to that skill directory.
- Read only the referenced files needed for the task.
- Do not carry a skill body across unrelated turns unless it remains in raw context or is re-read."#;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SkillError {
    #[error("{field} must not be blank")]
    Blank { field: &'static str },
    #[error("{field} must not contain control characters")]
    ControlCharacters { field: &'static str },
    #[error("skill path must be relative and must end with SKILL.md: {path}")]
    InvalidSkillPath { path: String },
    #[error("skill {name} is duplicated at {path}")]
    Duplicate { name: String, path: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillMetadata {
    name: String,
    description: String,
    skill_md_path: PathBuf,
    root: PathBuf,
}

impl SkillMetadata {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        skill_md_path: PathBuf,
        root: PathBuf,
    ) -> Result<Self, SkillError> {
        let name = name.into();
        validate_text("skill name", &name)?;
        let description = description.into();
        validate_text("skill description", &description)?;
        validate_skill_path(&skill_md_path)?;
        Ok(Self {
            name,
            description,
            skill_md_path,
            root,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    #[must_use]
    pub fn skill_md_path(&self) -> &std::path::Path {
        &self.skill_md_path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCatalog {
    skills: Vec<SkillMetadata>,
    warnings: Vec<SkillLoadWarning>,
}

impl SkillCatalog {
    pub fn from_metadata(skills: Vec<SkillMetadata>) -> Result<Self, SkillError> {
        let mut by_key = BTreeMap::new();
        for skill in skills {
            let key = normalized_skill_name(skill.name());
            match by_key.entry(key) {
                Entry::Vacant(entry) => {
                    entry.insert(skill);
                }
                Entry::Occupied(entry) => {
                    return Err(SkillError::Duplicate {
                        name: entry.get().name().to_owned(),
                        path: skill.skill_md_path.display().to_string(),
                    });
                }
            }
        }
        Ok(Self {
            skills: by_key.into_values().collect(),
            warnings: Vec::new(),
        })
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    #[must_use]
    pub fn skills(&self) -> &[SkillMetadata] {
        &self.skills
    }

    #[must_use]
    pub fn warnings(&self) -> &[SkillLoadWarning] {
        &self.warnings
    }

    pub(crate) fn to_stable_prefix_message_text(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let mut lines = vec![
            "## Skills".to_owned(),
            SKILLS_INTRO.to_owned(),
            "### Available skills".to_owned(),
        ];
        for skill in &self.skills {
            lines.push(format!(
                "- {}: {} (file: {})",
                skill.name,
                skill.description,
                skill.skill_md_path.display()
            ));
        }
        lines.push("### How to use skills".to_owned());
        lines.push(SKILLS_HOW_TO_USE.to_owned());
        Some(lines.join("\n"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillLoadWarning {
    path: PathBuf,
    message: String,
}

impl SkillLoadWarning {
    #[must_use]
    pub fn new(path: PathBuf, message: impl Into<String>) -> Self {
        Self {
            path,
            message: message.into(),
        }
    }
}

fn validate_text(field: &'static str, value: &str) -> Result<(), SkillError> {
    if value.trim().is_empty() {
        return Err(SkillError::Blank { field });
    }
    if value
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(SkillError::ControlCharacters { field });
    }
    Ok(())
}

fn validate_skill_path(path: &std::path::Path) -> Result<(), SkillError> {
    if path.is_absolute() || path.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
        return Err(SkillError::InvalidSkillPath {
            path: path.display().to_string(),
        });
    }
    Ok(())
}

fn normalized_skill_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}
```

If Rust reports an unused import, remove it instead of suppressing the warning.

- [ ] **Step 4: Run renderer tests**

Run:

```bash
cargo test -p merry-runtime skill
```

Expected: PASS.

- [ ] **Step 5: Commit**

Run:

```bash
git add crates/merry-runtime/src/lib.rs crates/merry-runtime/src/skill.rs
git commit -m "feat(runtime): add skill metadata catalog"
```

## Task 2: Load SKILL.md Frontmatter From Skill Roots

**Files:**
- Modify: `crates/merry-runtime/src/skill.rs`
- Modify: `crates/merry-runtime/Cargo.toml`

- [ ] **Step 1: Add failing loader tests**

Append tests to `crates/merry-runtime/src/skill.rs`:

```rust
#[cfg(test)]
mod loader_tests {
    use super::*;
    use std::{fs, path::Path};

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().expect("test path has parent")).expect("mkdir");
        fs::write(path, text).expect("write");
    }

    #[test]
    fn loads_skill_metadata_from_roots() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("skills");
        write(
            &root.join("frontend/SKILL.md"),
            r#"---
name: frontend-design
description: Use when building polished frontend UI.
---

# Frontend Design

full skill body sentinel
"#,
        );

        let catalog = SkillCatalog::load_from_roots([root.clone()]).expect("loads catalog");
        assert_eq!(catalog.skills().len(), 1);
        assert_eq!(catalog.skills()[0].name(), "frontend-design");
        assert_eq!(
            catalog.skills()[0].description(),
            "Use when building polished frontend UI."
        );
        assert_eq!(
            catalog.skills()[0].skill_md_path(),
            Path::new("frontend/SKILL.md")
        );
        assert!(catalog.warnings().is_empty());
    }

    #[test]
    fn skips_invalid_skill_frontmatter_with_warning() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("skills");
        write(
            &root.join("valid/SKILL.md"),
            "---\nname: valid\n description: bad indentation\n---\n",
        );
        write(
            &root.join("missing-description/SKILL.md"),
            "---\nname: missing-description\n---\n",
        );
        write(
            &root.join("ok/SKILL.md"),
            "---\nname: ok\ndescription: Valid skill.\n---\n# OK\n",
        );

        let catalog = SkillCatalog::load_from_roots([root]).expect("load should not fail");
        assert_eq!(catalog.skills().len(), 1);
        assert_eq!(catalog.skills()[0].name(), "ok");
        assert_eq!(catalog.warnings().len(), 2);
    }
}
```

- [ ] **Step 2: Add test dependency**

Add to `crates/merry-runtime/Cargo.toml`:

```toml
[dev-dependencies]
tempfile.workspace = true
```

- [ ] **Step 3: Run tests and confirm failure**

Run:

```bash
cargo test -p merry-runtime skill::loader_tests
```

Expected: FAIL because `SkillCatalog::load_from_roots` is missing.

- [ ] **Step 4: Implement bounded root scan and frontmatter parser**

Implement:

```rust
impl SkillCatalog {
    pub fn load_from_roots<I>(roots: I) -> Result<Self, SkillError>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut skills = Vec::new();
        let mut warnings = Vec::new();
        for root in roots {
            scan_root(&root, &mut skills, &mut warnings)?;
        }
        let mut catalog = Self::from_metadata(skills)?;
        catalog.warnings = warnings;
        Ok(catalog)
    }
}
```

Keep the first implementation deliberately small:

- Recursively scan directories under each root.
- Hard cap scan depth at `6`.
- Hard cap inspected directories at `2000` per root.
- Recognize files named exactly `SKILL.md`.
- Parse frontmatter by splitting the first `--- ... ---` block.
- Parse only `name: ...` and `description: ...` single-line fields.
- Skip invalid files with `SkillLoadWarning`.
- Return a `SkillError` only for root-level failures that make the configured root unusable.

Add these helper variants:

```rust
#[error("skill root does not exist: {root}")]
RootNotFound { root: String },
#[error("skill root is not a directory: {root}")]
RootNotDirectory { root: String },
#[error("could not read skill root {root}: {message}")]
RootRead { root: String, message: String },
```

Do not add `serde_yaml` in this slice. The MVP parser only needs the standard fields used by current tests.

- [ ] **Step 5: Run loader tests**

Run:

```bash
cargo test -p merry-runtime skill::loader_tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/merry-runtime/src/skill.rs crates/merry-runtime/Cargo.toml
git commit -m "feat(runtime): load skills from roots"
```

## Task 3: Insert Skill Metadata Into Stable Prefix

**Files:**
- Modify: `crates/merry-runtime/src/runtime.rs`
- Modify: `crates/merry-runtime/src/session.rs`
- Modify: `crates/merry-runtime/src/step.rs`
- Modify: `crates/merry-runtime/tests/provider_boundary.rs`

- [ ] **Step 1: Add failing provider-boundary test**

Add this test near `project_rules_enter_stable_prefix_and_affect_stable_hash`:

```rust
#[tokio::test(flavor = "current_thread")]
async fn compiled_provider_request_skill_metadata_enters_stable_prefix_before_project_rules() {
    let provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let skill_catalog = SkillCatalog::from_metadata(vec![SkillMetadata::new(
        "frontend-design",
        "Use when building polished frontend UI.",
        PathBuf::from("skills/frontend-design/SKILL.md"),
        PathBuf::from("/workspace"),
    )
    .expect("valid skill metadata")])
    .expect("valid skill catalog");
    let runtime = Runtime::builder(session_id("provider-skill-prefix"))
        .skill_catalog(skill_catalog)
        .project_rules(
            ProjectRules::new("AGENTS.md", "Use project rules sentinel.\n")
                .expect("valid project rules"),
        )
        .model_provider(Arc::new(provider.clone()), model_name())
        .build()
        .expect("runtime should build");

    collect_step(&runtime, "Use the skill list.").await;
    let request = provider.recorded_requests()[0].clone();

    assert_eq!(request.stable_prefix_message_count(), 3);
    assert_eq!(request.stable_prefix_messages().len(), 3);
    assert!(
        request.stable_prefix_messages()[0]
            .content()
            .as_text()
            .contains("You are Merry")
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("## Skills")
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("workspace_read_file")
    );
    assert!(
        request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("skills/frontend-design/SKILL.md")
    );
    assert!(
        !request.stable_prefix_messages()[1]
            .content()
            .as_text()
            .contains("full skill body sentinel")
    );
    assert!(
        request.stable_prefix_messages()[2]
            .content()
            .as_text()
            .contains("project-rules-source:AGENTS.md")
    );
}
```

Add imports at the top of the test file if missing:

```rust
use merry_runtime::{ProjectRules, SkillCatalog, SkillMetadata};
use std::path::PathBuf;
```

- [ ] **Step 2: Run test and confirm failure**

Run:

```bash
cargo test -p merry-runtime compiled_provider_request_skill_metadata_enters_stable_prefix_before_project_rules
```

Expected: FAIL because `RuntimeBuilder::skill_catalog` is missing.

- [ ] **Step 3: Wire catalog through runtime/session/request inputs**

Add to `RuntimeBuilder`:

```rust
skill_catalog: Option<SkillCatalog>,
```

Add builder method:

```rust
#[must_use]
pub fn skill_catalog(mut self, skill_catalog: SkillCatalog) -> Self {
    self.skill_catalog = Some(skill_catalog);
    self
}
```

Store the catalog on `SessionState` beside `project_rules`:

```rust
skill_catalog: Option<SkillCatalog>,
```

Add methods:

```rust
pub(crate) fn set_skill_catalog(&mut self, skill_catalog: SkillCatalog) {
    self.skill_catalog = Some(skill_catalog);
}

pub(crate) fn skill_catalog(&self) -> Option<SkillCatalog> {
    self.skill_catalog.clone()
}
```

Pass it through `StepRequestInputs` and `StepModelRequestParts`.

- [ ] **Step 4: Update stable-prefix assembly**

In `compile_step_model_request`, change:

```rust
let stable_prefix_message_count = 1 + usize::from(project_rules.is_some());
```

to count non-empty skill catalog:

```rust
let skill_metadata_text = skill_catalog
    .and_then(|catalog| catalog.to_stable_prefix_message_text());
let stable_prefix_message_count =
    1 + usize::from(skill_metadata_text.is_some()) + usize::from(project_rules.is_some());
```

Push messages in this order:

```rust
messages.push(base_runtime_instructions);
if let Some(skill_metadata_text) = skill_metadata_text {
    messages.push(system(skill_metadata_text));
}
if let Some(project_rules) = project_rules {
    messages.push(project_rules_message);
}
```

Do not put skills in `CompiledContext::to_snapshot()`.

- [ ] **Step 5: Run focused provider-boundary tests**

Run:

```bash
cargo test -p merry-runtime compiled_provider_request_skill_metadata_enters_stable_prefix_before_project_rules
cargo test -p merry-runtime project_rules_enter_stable_prefix_and_affect_stable_hash
cargo test -p merry-runtime compiled_provider_request_stable_prefix_hash_tracks_base_instructions_and_tools_only
```

Expected: PASS after updating any test names/assertion text that still says "base instructions and tools only".

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/merry-runtime/src/runtime.rs crates/merry-runtime/src/session.rs crates/merry-runtime/src/step.rs crates/merry-runtime/tests/provider_boundary.rs
git commit -m "feat(runtime): project skills in stable prefix"
```

## Task 4: Prove Prefix Hash Behavior And Dynamic Body Isolation

**Files:**
- Modify: `crates/merry-runtime/tests/provider_boundary.rs`

- [ ] **Step 1: Add failing hash/isolation test**

Add:

```rust
#[tokio::test(flavor = "current_thread")]
async fn skill_metadata_changes_stable_prefix_but_not_dynamic_context() {
    let first_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let first_catalog = SkillCatalog::from_metadata(vec![SkillMetadata::new(
        "frontend-design",
        "Use for UI work.",
        PathBuf::from("skills/frontend-design/SKILL.md"),
        PathBuf::from("/workspace"),
    )
    .expect("valid skill metadata")])
    .expect("valid catalog");
    let first_runtime = Runtime::builder(session_id("provider-skill-hash-first"))
        .skill_catalog(first_catalog)
        .model_provider(Arc::new(first_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&first_runtime, "Same dynamic input.").await;
    let first_request = first_provider.recorded_requests()[0].clone();

    let changed_provider = FakeModelProvider::new(vec![Ok(completed_event())]);
    let changed_catalog = SkillCatalog::from_metadata(vec![SkillMetadata::new(
        "frontend-design",
        "Use for UI and responsive layout work.",
        PathBuf::from("skills/frontend-design/SKILL.md"),
        PathBuf::from("/workspace"),
    )
    .expect("valid skill metadata")])
    .expect("valid catalog");
    let changed_runtime = Runtime::builder(session_id("provider-skill-hash-changed"))
        .skill_catalog(changed_catalog)
        .model_provider(Arc::new(changed_provider.clone()), model_name())
        .build()
        .expect("runtime should build");
    collect_step(&changed_runtime, "Same dynamic input.").await;
    let changed_request = changed_provider.recorded_requests()[0].clone();

    assert_ne!(
        first_request.stable_prefix_hash(),
        changed_request.stable_prefix_hash()
    );
    assert_eq!(
        first_request.dynamic_context_hash(),
        changed_request.dynamic_context_hash()
    );
}
```

- [ ] **Step 2: Run test**

Run:

```bash
cargo test -p merry-runtime skill_metadata_changes_stable_prefix_but_not_dynamic_context
```

Expected: PASS if Task 3 was implemented correctly. If it fails, fix the stable-prefix count/order instead of moving skills into dynamic context.

- [ ] **Step 3: Commit**

Run:

```bash
git add crates/merry-runtime/tests/provider_boundary.rs
git commit -m "test(runtime): cover skill prefix hashing"
```

## Task 5: Parse Skill Config Roots

**Files:**
- Modify: `crates/merry-cli/src/config.rs`
- Modify: `examples/config.toml`

- [ ] **Step 1: Add failing config tests**

Add to `crates/merry-cli/src/config.rs` tests:

```rust
#[test]
fn parses_skill_config_roots() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[skills]
enabled = true
roots = ["skills", "~/shared-skills", "/opt/company/skills"]
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let skills = config.skill_roots().expect("skill roots should resolve");
    assert_eq!(
        skills,
        vec![
            PathBuf::from("/home/alice/.config/merry/skills"),
            PathBuf::from("/home/alice/shared-skills"),
            PathBuf::from("/opt/company/skills"),
        ]
    );
}

#[test]
fn disabled_or_missing_skills_return_no_roots() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let missing = MerryConfig::load_optional_from_text(Some(""), &paths)
        .expect("config should parse")
        .expect("config should be present");
    assert_eq!(missing.skill_roots().expect("missing skills is valid"), Vec::<PathBuf>::new());

    let disabled = MerryConfig::load_optional_from_text(
        Some("[skills]\nenabled = false\nroots = [\"skills\"]\n"),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");
    assert_eq!(disabled.skill_roots().expect("disabled skills is valid"), Vec::<PathBuf>::new());
}
```

- [ ] **Step 2: Run tests and confirm failure**

Run:

```bash
cargo test -p merry-cli config::tests::parses_skill_config_roots
```

Expected: FAIL because `[skills]` is denied as an unknown config field or `skill_roots` is missing.

- [ ] **Step 3: Implement config parsing**

Add to `MerryConfigToml`:

```rust
skills: Option<SkillsToml>,
```

Add:

```rust
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SkillsToml {
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    roots: Vec<String>,
}
```

Add method:

```rust
pub fn skill_roots(&self) -> Result<Vec<PathBuf>, ConfigError> {
    let Some(skills) = self.raw.skills.as_ref() else {
        return Ok(Vec::new());
    };
    if !skills.enabled {
        return Ok(Vec::new());
    }
    let mut roots = Vec::with_capacity(skills.roots.len());
    for root in &skills.roots {
        if root.trim().is_empty() {
            return Err(ConfigError::Invalid("skills.roots entries must not be blank".to_owned()));
        }
        roots.push(resolve_config_relative_path(root, &self.config_dir)?);
    }
    Ok(roots)
}
```

- [ ] **Step 4: Update example config**

Add to `examples/config.toml` after `[global]`:

```toml
[skills]
# Skill metadata is projected into the cacheable stable prefix. Full SKILL.md
# bodies remain on disk and are read through workspace_read_file only when used.
enabled = false
roots = [
  "skills",
]
```

Update `example_config_toml_matches_current_schema_and_resolves_user_defaults`:

```rust
assert_eq!(
    config.skill_roots().expect("example skill roots should validate"),
    Vec::<PathBuf>::new()
);
```

- [ ] **Step 5: Run config tests**

Run:

```bash
cargo test -p merry-cli config::tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/merry-cli/src/config.rs examples/config.toml
git commit -m "feat(cli): parse skill roots config"
```

## Task 6: Wire Skill Metadata Into Coding-Loop Runtime

**Files:**
- Modify: `crates/merry-cli/src/main.rs`
- Modify: `crates/merry-cli/src/config.rs`

- [ ] **Step 1: Add failing coding-loop prefix test**

Add around the existing coding-loop runtime builder tests:

```rust
#[tokio::test(flavor = "current_thread")]
async fn coding_loop_runtime_projects_skill_metadata_without_body() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let skill_root = temp.path().join("skills");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::create_dir_all(skill_root.join("demo")).expect("mkdir skill");
    std::fs::write(
        skill_root.join("demo/SKILL.md"),
        "---\nname: demo-skill\ndescription: Use for demo tasks.\n---\n# Demo\nbody sentinel\n",
    )
    .expect("write skill");

    let provider = ScriptedProvider::new(vec![vec![Ok(ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text("done")], FinishReason::Stop, None),
    })]]);
    let runner = Arc::new(FakeProcessRunner::succeeding(""));
    let runtime = super::build_coding_loop_runtime(
        "coding-loop-skill-prefix",
        &workspace,
        AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        Arc::new(provider.clone()),
        ModelName::new("debug-model").unwrap(),
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            context_compaction: None,
            skill_roots: vec![skill_root.clone()],
        },
    )
    .expect("runtime should build");

    collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Inspect skills.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should complete");
    let request = provider.recorded_requests()[0].clone();
    let stable_text = request
        .stable_prefix_messages()
        .iter()
        .map(|message| message.content().as_text())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(stable_text.contains("demo-skill"));
    assert!(stable_text.contains("Use for demo tasks."));
    assert!(stable_text.contains("workspace_read_file"));
    assert!(stable_text.contains("demo/SKILL.md"));
    assert!(!stable_text.contains("body sentinel"));
}
```

- [ ] **Step 2: Run test and confirm failure**

Run:

```bash
cargo test -p merry-cli coding_loop_runtime_projects_skill_metadata_without_body
```

Expected: FAIL because `CodingLoopRuntimeOptions` has no `skill_roots` and the coding-loop builder does not load a skill catalog yet.

- [ ] **Step 3: Add skill roots to coding-loop options**

Extend:

```rust
struct CodingLoopRuntimeOptions {
    allow_hidden_workspace_paths: bool,
    automatic_compaction: AutomaticCompactionConfig,
    context_compaction: Option<RuntimeRoleProviderConfig>,
    skill_roots: Vec<PathBuf>,
}
```

Set `skill_roots: Vec::new()` in deterministic smoke constructors.

For config-backed live smoke entry points, resolve roots before building the runtime:

```rust
let skill_roots = merry_config
    .map(MerryConfig::skill_roots)
    .transpose()
    .map_err(unexpected)?
    .unwrap_or_default();
```

Pass `skill_roots` into `CodingLoopRuntimeOptions`.

- [ ] **Step 4: Apply skill catalog in coding-loop builder**

In `build_coding_loop_runtime`, after base builder creation and before `WorkspaceCodingLoopProfile::new(...)`, add:

```rust
if !options.skill_roots.is_empty() {
    let catalog = merry_runtime::SkillCatalog::load_from_roots(options.skill_roots.clone())
        .map_err(unexpected)?;
    builder = builder.skill_catalog(catalog);
}
```

This intentionally keeps skills out of generic debug runtime paths that do not register `workspace_read_file`. Invalid individual `SKILL.md` files are skipped with warnings inside the catalog; unreadable configured roots fail runtime construction.

- [ ] **Step 5: Run coding-loop prefix test**

Run:

```bash
cargo test -p merry-cli coding_loop_runtime_projects_skill_metadata_without_body
```

Expected: PASS.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/merry-cli/src/main.rs
git commit -m "feat(cli): apply coding-loop skills"
```

## Task 7: Make Coding-Loop Workspace Tools Able To Read Skill Roots

**Files:**
- Modify: `crates/merry-cli/src/main.rs`

- [ ] **Step 1: Add failing coding-loop runtime test**

Add a test around existing coding-loop runtime builder tests:

```rust
#[tokio::test(flavor = "current_thread")]
async fn coding_loop_runtime_includes_skill_roots_in_workspace_read_tools() {
    let temp = tempfile::tempdir().expect("tempdir");
    let workspace = temp.path().join("workspace");
    let skill_root = temp.path().join("skills");
    std::fs::create_dir_all(&workspace).expect("mkdir workspace");
    std::fs::create_dir_all(skill_root.join("demo")).expect("mkdir skill");
    std::fs::write(
        skill_root.join("demo/SKILL.md"),
        "---\nname: demo-skill\ndescription: Demo skill.\n---\n# Demo\n",
    )
    .expect("write skill");

    let provider = ScriptedProvider::new(vec![vec![Ok(coding_loop_workspace_call(
        "call-read-skill",
        WORKSPACE_READ_FILE_TOOL,
        [("path", serde_json::Value::String("demo/SKILL.md".to_owned()))],
    )
    .expect("workspace read call should build"))]]);
    let runner = Arc::new(FakeProcessRunner::succeeding(""));
    let runtime = super::build_coding_loop_runtime(
        "coding-loop-skill-root-read",
        &workspace,
        AcceptedLocalWorkspaceProcessAdmission::accept_cli_bwrap_v1(),
        Arc::new(provider.clone()),
        ModelName::new("debug-model").unwrap(),
        runner,
        CodingLoopRuntimeOptions {
            allow_hidden_workspace_paths: false,
            automatic_compaction: merry_runtime::AutomaticCompactionConfig::disabled(),
            context_compaction: None,
            skill_roots: vec![skill_root.clone()],
        },
    )
    .expect("runtime should build");

    let events = collect_runtime_step_events(
        &runtime,
        StepInput::user_text("Read demo skill.").expect("valid input"),
        StepContext::default(),
    )
    .await
    .expect("runtime step should collect pending skill read");
    let pending = first_pending_tool_call(&events).expect("pending skill read");
    let execution_events = runtime
        .execute_tool_call(pending.id(), ToolExecutionContext::default())
        .await
        .expect("skill read should execute");
    let result = resolved_tool_result(&execution_events);
    assert_eq!(result.status(), ToolCallResultStatus::Succeeded);
}
```

Add `WORKSPACE_READ_FILE_TOOL` to the existing `merry_tool_workspace` imports in the test module:

```rust
use merry_tool_workspace::{
    WORKSPACE_PATCH_TOOL, WORKSPACE_READ_FILE_TOOL, WorkspaceCodingLoopProfile,
    WorkspaceToolsConfig,
};
```

This uses the existing coding-loop workspace tool path. Do not add a skill-specific read tool to make this test pass.

- [ ] **Step 2: Run test and confirm failure**

Run:

```bash
cargo test -p merry-cli coding_loop_runtime_includes_skill_roots_in_workspace_read_tools
```

Expected: FAIL because workspace tools only know the fixture root, so `demo/SKILL.md` under the skill root is not readable yet.

- [ ] **Step 3: Add skill roots to coding-loop workspace roots**

In `build_coding_loop_runtime`, build workspace roots:

```rust
let mut workspace_roots = vec![root.to_path_buf()];
workspace_roots.extend(options.skill_roots.iter().cloned());
```

Use:

```rust
WorkspaceToolsConfig::new(workspace_roots)
```

instead of only `root.to_path_buf()`.

Do not add `skill_read`. The same `workspace_read_file` executor should read configured skill roots.

- [ ] **Step 4: Run coding-loop tests**

Run:

```bash
cargo test -p merry-cli coding_loop_runtime_includes_skill_roots_in_workspace_read_tools
cargo test -p merry-cli coding_loop_task
```

Expected: PASS.

- [ ] **Step 5: Commit**

Run:

```bash
git add crates/merry-cli/src/main.rs
git commit -m "feat(cli): expose skill roots to workspace tools"
```

## Task 8: Final Verification And Spec Sync

**Files:**
- Modify: `specs/2026-05-31-skill-progressive-disclosure.md` only if implementation revealed a factual correction.

- [ ] **Step 1: Run focused checks**

Run:

```bash
cargo test -p merry-runtime skill
cargo test -p merry-runtime compiled_provider_request_skill_metadata
cargo test -p merry-cli config::tests
cargo test -p merry-cli coding_loop_runtime_projects_skill_metadata_without_body
cargo test -p merry-cli coding_loop_runtime_includes_skill_roots_in_workspace_read_tools
```

Expected: all PASS.

- [ ] **Step 2: Run broad checks**

Run:

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all
```

Expected: all PASS.

- [ ] **Step 3: Review spec for drift**

Open `specs/2026-05-31-skill-progressive-disclosure.md` and confirm it still matches implementation:

- Skill metadata is stable prefix.
- Full body is only read through `workspace_read_file`.
- No `skill_read`.
- No `trigger`.
- No runtime semantic selector.

Only edit the spec if the implementation made one of these statements false or more precise.

- [ ] **Step 4: Commit final sync if needed**

If Task 8 changed docs or fixed final verification issues:

```bash
git add specs/2026-05-31-skill-progressive-disclosure.md
git commit -m "docs: sync skill progressive disclosure spec"
```

If no files changed, do not create an empty commit.

## Execution Notes

- Keep commits focused. The expected commit sequence is one commit per task.
- Do not update `ROADMAP.md` in this plan.
- Do not add `skill_read`.
- Do not add `trigger`.
- Do not place skill metadata in `CompiledContext::to_snapshot()`.
- If a configured skill root is unreadable, fail runtime construction with an actionable error; if an individual skill file is malformed, skip it and keep a warning on the catalog.
- If `workspace_read_file` path ambiguity appears because multiple roots contain the same relative path, do not solve that with `skill_read` in this plan. Prefer documenting the limitation and using root ordering consistently.
