use crate::{coding_agent, search_tools::SearchToolAvailability};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

/// Builds a temporary PATH-style directory holding the requested programs.
fn tool_directory(programs: &[&str]) -> (tempfile::TempDir, String) {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin).expect("bin directory should be created");
    for program in programs {
        let path = bin.join(program);
        fs::write(&path, b"#!/bin/sh\n").expect("program stub should be written");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
                .expect("program stub should be executable");
        }
    }
    let path = bin.to_string_lossy().into_owned();
    (temp, path)
}

#[test]
fn probe_reports_the_modern_search_tools_on_the_action_path() {
    let (_temp, path) = tool_directory(&["rg", "fd"]);
    let availability = SearchToolAvailability::detect(OsStr::new(&path), Path::exists);
    let summary = availability.summary_line();

    assert!(summary.contains("`rg` and `fd` are installed"), "{summary}");
    assert!(!summary.contains("not installed"), "{summary}");
}

#[test]
fn probe_accepts_the_distribution_name_of_fd() {
    let (_temp, path) = tool_directory(&["rg", "fdfind"]);
    let availability = SearchToolAvailability::detect(OsStr::new(&path), Path::exists);

    assert!(
        availability
            .summary_line()
            .contains("`rg` and `fd` are installed")
    );
}

#[test]
fn probe_names_the_missing_tool_instead_of_promising_it() {
    let (_temp, path) = tool_directory(&["rg"]);
    let summary = SearchToolAvailability::detect(OsStr::new(&path), Path::exists).summary_line();

    assert!(summary.contains("`rg` is installed"), "{summary}");
    assert!(
        summary.contains("Neither `fd` nor `fdfind` is installed"),
        "{summary}"
    );
}

#[test]
fn probe_reports_that_no_modern_search_tool_is_installed() {
    let (_temp, path) = tool_directory(&[]);
    let summary = SearchToolAvailability::detect(OsStr::new(&path), Path::exists).summary_line();

    assert!(
        summary.starts_with("Action PATH search tools: neither `rg` nor `fd` is installed"),
        "{summary}"
    );
}

#[test]
fn probe_builds_candidates_only_from_named_path_directories() {
    let (_temp, path) = tool_directory(&["rg"]);
    let mut probed = Vec::<PathBuf>::new();
    let mixed_path = format!("::{path}:");
    let availability = SearchToolAvailability::detect(OsStr::new(&mixed_path), |candidate| {
        probed.push(candidate.to_path_buf());
        candidate.exists()
    });

    assert!(
        availability.summary_line().contains("`rg` is installed"),
        "{probed:?}"
    );
    assert_eq!(
        probed,
        vec![
            Path::new(&path).join("rg"),
            Path::new(&path).join("fd"),
            Path::new(&path).join("fdfind"),
        ]
    );
}

#[test]
fn coding_profile_reports_search_tool_availability_for_this_host() {
    let temp = tempfile::tempdir().expect("tempdir should be created");
    let profile = coding_agent(temp.path())
        .build()
        .expect("coding-agent profile should build");
    let summary = profile
        .runtime_profile()
        .initial_context_summaries()
        .get("project-capabilities")
        .expect("profile should seed the project capability summary")
        .clone();

    assert!(summary.contains("Coding file capabilities:"), "{summary}");
    assert!(summary.contains("Action PATH search tools:"), "{summary}");
}
