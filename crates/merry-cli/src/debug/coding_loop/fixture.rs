use crate::cli_error::{CliError, unexpected};
use crate::debug::CodingLoopTaskSmokeTask;
use crate::debug::coding_loop::{
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE, CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL,
    CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID,
};
use std::{env, fs, path::PathBuf};

pub(crate) fn prepare_coding_loop_smoke_fixture(name: &str) -> Result<PathBuf, CliError> {
    let root = env::current_dir()
        .map_err(unexpected)?
        .join(".merry")
        .join("local")
        .join(name);
    if root.exists() {
        fs::remove_dir_all(&root).map_err(unexpected)?;
    }
    fs::create_dir_all(root.join("src")).map_err(unexpected)?;
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"merry-coding-loop-smoke\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n",
    )
    .map_err(unexpected)?;
    fs::write(root.join("src/lib.rs"), coding_loop_smoke_initial_source()).map_err(unexpected)?;
    Ok(root)
}

pub(crate) fn prepare_coding_loop_subagent_live_smoke_fixture() -> Result<PathBuf, CliError> {
    let root = prepare_coding_loop_smoke_fixture(CODING_LOOP_SUBAGENT_LIVE_SMOKE_SESSION_ID)?;
    fs::write(
        root.join(CODING_LOOP_SUBAGENT_LIVE_SMOKE_FILE),
        CODING_LOOP_SUBAGENT_LIVE_SMOKE_INITIAL,
    )
    .map_err(unexpected)?;
    Ok(root)
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CodingLoopTaskSmokeFixture {
    task: CodingLoopTaskSmokeTask,
}

impl CodingLoopTaskSmokeFixture {
    pub(crate) const fn for_task(task: CodingLoopTaskSmokeTask) -> Self {
        Self { task }
    }

    pub(crate) const fn package_name(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => "merry-coding-loop-task-status-text",
        }
    }

    pub(crate) const fn crate_name(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => "merry_coding_loop_task_status_text",
        }
    }

    pub(crate) fn initial_source(self) -> String {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => status_text_fixture_source("todo"),
        }
    }

    pub(crate) fn patched_source(self) -> String {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => status_text_fixture_source("done"),
        }
    }

    pub(crate) const fn patch_remove_line(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                "    Entry { key: \"status\", value: \"todo\" },"
            }
        }
    }

    pub(crate) const fn patch_add_line(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                "    Entry { key: \"status\", value: \"done\" },"
            }
        }
    }

    pub(crate) fn patch_text(self) -> String {
        format!(
            "*** Begin Workspace Patch\n*** Update File: src/lib.rs\n-{}\n+{}\n*** End Workspace Patch",
            self.patch_remove_line(),
            self.patch_add_line()
        )
    }

    pub(crate) const fn test_source(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                "Expected behavior: src/lib.rs exposes status() returning the completed status text.\n"
            }
        }
    }

    pub(crate) fn agents_source(self) -> String {
        format!(
            "\
# AGENTS.md

This is a disposable Rust fixture used by Merry's live coding-loop smoke.

Project rules:
- Inspect the repository before editing.
- Read `tests/status.rs` to understand the required behavior.
- Fix implementation code in `src/lib.rs`; do not edit tests or `Cargo.toml`.
- Use a localized source edit; do not rewrite whole files for small fixes.
- After editing, run `cargo check -p {package}` and `cargo test -p {package}`.
- Report the checks you ran and whether they passed.
",
            package = self.package_name()
        )
    }

    pub(crate) fn integration_test_source(self) -> String {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => format!(
                "use {}::{{default_status, preview_status, status}};\n\n#[test]\nfn status_returns_done_without_changing_related_entries() {{\n    assert_eq!(default_status(), \"todo\");\n    assert_eq!(status(), \"done\");\n    assert_eq!(preview_status(), \"todo\");\n}}\n",
                self.crate_name()
            ),
        }
    }

    pub(crate) const fn task_prompt(self) -> &'static str {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                "Fix the disposable Rust fixture so status() returns done. Inspect the workspace, verify the target text is initially missing from src/lib.rs with rg, read the source, apply one constrained patch, run rg again on src/lib.rs to verify, and then report the result."
            }
        }
    }

    pub(crate) fn live_task_prompt(self, _relative_cwd: Option<&str>) -> String {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                "Fix this disposable Rust project so the required status-text behavior is implemented. Use the available tools to inspect, edit, and verify before reporting completion.".to_owned()
            }
        }
    }

    pub(crate) fn source_satisfies_task(self, source: &str) -> bool {
        match self.task {
            CodingLoopTaskSmokeTask::StatusText => {
                source.contains("Entry { key: \"default\", value: \"todo\" },")
                    && source.contains("Entry { key: \"status\", value: \"done\" },")
                    && source.contains("Entry { key: \"preview\", value: \"todo\" },")
                    && !source.contains("Entry { key: \"status\", value: \"todo\" },")
            }
        }
    }
}

fn status_text_fixture_source(status: &str) -> String {
    let mut source = format!(
        "#[derive(Debug, Clone, Copy)]\nstruct Entry {{\n    key: &'static str,\n    value: &'static str,\n}}\n\nconst ENTRIES: &[Entry] = &[\n    Entry {{ key: \"default\", value: \"todo\" }},\n    Entry {{ key: \"status\", value: \"{status}\" }},\n    Entry {{ key: \"preview\", value: \"todo\" }},\n];\n\npub fn default_status() -> &'static str {{\n    resolve(\"default\")\n}}\n\npub fn status() -> &'static str {{\n    resolve(\"status\")\n}}\n\npub fn preview_status() -> &'static str {{\n    resolve(\"preview\")\n}}\n\nfn resolve(key: &str) -> &'static str {{\n    ENTRIES\n        .iter()\n        .find(|entry| entry.key == key)\n        .map(|entry| entry.value)\n        .unwrap_or(\"missing\")\n}}\n\n"
    );
    for index in 1..=30 {
        source.push_str(&format!(
            "pub fn fixture_note_{index:03}() -> &'static str {{ \"context-{index:03}\" }}\n"
        ));
    }
    source
}

pub(crate) fn prepare_coding_loop_task_fixture(
    name: &str,
    fixture: CodingLoopTaskSmokeFixture,
) -> Result<PathBuf, CliError> {
    let root = env::current_dir()
        .map_err(unexpected)?
        .join(".merry")
        .join("local")
        .join(name);
    if root.exists() {
        fs::remove_dir_all(&root).map_err(unexpected)?;
    }
    fs::create_dir_all(root.join("src")).map_err(unexpected)?;
    fs::create_dir_all(root.join("tests")).map_err(unexpected)?;
    fs::write(
        root.join("Cargo.toml"),
        coding_loop_task_fixture_manifest(fixture),
    )
    .map_err(unexpected)?;
    fs::write(root.join("src/lib.rs"), fixture.initial_source()).map_err(unexpected)?;
    fs::write(root.join("AGENTS.md"), fixture.agents_source()).map_err(unexpected)?;
    fs::write(
        root.join("tests/status.rs"),
        fixture.integration_test_source(),
    )
    .map_err(unexpected)?;
    fs::write(root.join("tests.md"), fixture.test_source()).map_err(unexpected)?;
    Ok(root)
}

pub(crate) fn coding_loop_task_fixture_manifest(fixture: CodingLoopTaskSmokeFixture) -> String {
    format!(
        "[package]\nname = \"{}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\npath = \"src/lib.rs\"\n\n[workspace]\n",
        fixture.package_name()
    )
}

pub(crate) fn coding_loop_smoke_initial_source() -> &'static str {
    "pub fn greeting() -> &'static str {\n    \"unfixed\"\n}\n"
}

pub(crate) fn coding_loop_smoke_patched_source() -> &'static str {
    "pub fn greeting() -> &'static str {\n    \"fixed-by-live-llm\"\n}\n"
}
