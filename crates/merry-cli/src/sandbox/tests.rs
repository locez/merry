use crate::{
    config::XdgPaths,
    sandbox::{
        Bootstrap, ClipboardAccess, Error, Plan,
        host::{Host, HostPathKind, HostPathMetadata, HostPathProbe},
        integrations::{GraphicalEnvironment, HostIntegrationEnvironment},
        os, plan_bootstrap_with_file_exists, plan_bootstrap_with_probe,
    },
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Default)]
struct FakeHostProbe {
    metadata: BTreeMap<PathBuf, HostPathMetadata>,
}

impl FakeHostProbe {
    fn socket(mut self, path: &str, owner_uid: u32) -> Self {
        self.metadata.insert(
            PathBuf::from(path),
            HostPathMetadata::new(HostPathKind::UnixSocket, owner_uid),
        );
        self
    }

    fn regular_file(mut self, path: &str, owner_uid: u32) -> Self {
        self.metadata.insert(
            PathBuf::from(path),
            HostPathMetadata::new(HostPathKind::RegularFile, owner_uid),
        );
        self
    }

    fn other(mut self, path: &str, owner_uid: u32) -> Self {
        self.metadata.insert(
            PathBuf::from(path),
            HostPathMetadata::new(HostPathKind::Other, owner_uid),
        );
        self
    }
}

impl HostPathProbe for FakeHostProbe {
    fn file_exists(&self, path: &Path) -> bool {
        path_is_fake_bwrap(path) || self.metadata.contains_key(path)
    }

    fn metadata(&self, path: &Path) -> Option<HostPathMetadata> {
        self.metadata.get(path).copied()
    }
}

fn sandbox_host() -> Host {
    Host {
        cwd: PathBuf::from("/workspace/merry"),
        current_exe: PathBuf::from("/workspace/merry/target/debug/merry"),
        args: vec![
            os("--with-sandbox"),
            os("debug"),
            os("--session-id"),
            os("custom-session"),
        ],
        path: Some(os("/custom/bin:/usr/bin")),
        openai_debug: None,
        inside_sandbox: false,
        xdg_paths: XdgPaths::from_parts(
            PathBuf::from("/home/alice"),
            Some(PathBuf::from("/host/config")),
            Some(PathBuf::from("/host/state")),
        ),
        log_settings: None,
        trusted_path_rules: Vec::new(),
        graphical_environment: GraphicalEnvironment::default(),
        host_integrations: Vec::new(),
        host_integration_environment: HostIntegrationEnvironment::default(),
        development_environment: Vec::new(),
        current_uid: 1_000,
    }
}

fn path_is_fake_bwrap(path: &Path) -> bool {
    path == Path::new("/custom/bin/bwrap")
}

fn plan_sandbox(with_sandbox: bool, host: &Host) -> Result<Bootstrap, Error> {
    plan_bootstrap_with_file_exists(with_sandbox, host, path_is_fake_bwrap)
}

fn plan_sandbox_with_clipboard(
    host: &Host,
    probe: &impl HostPathProbe,
) -> Result<Bootstrap, Error> {
    plan_bootstrap_with_probe(true, ClipboardAccess::Tui, host, probe)
}

fn plan_args(plan: &Plan) -> Vec<String> {
    plan.args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn assert_ro_mount(args: &[String], flag: &str, source: &str, destination: &str) {
    let resolved_source = merry_process::resolve_bwrap_path(Path::new(source));
    assert!(
        contains_sequence(
            args,
            &[
                flag,
                resolved_source.to_str().expect("UTF-8 test path"),
                destination,
            ],
        ),
        "missing mount {flag} {} {destination}",
        resolved_source.display()
    );
}

fn contains_sequence(args: &[String], expected: &[&str]) -> bool {
    sequence_position(args, expected).is_some()
}

fn sequence_position(args: &[String], expected: &[&str]) -> Option<usize> {
    args.windows(expected.len()).position(|window| {
        window
            .iter()
            .map(String::as_str)
            .eq(expected.iter().copied())
    })
}

mod bootstrap;

mod environment;

mod host_integrations;

mod mount_rules;

mod runtime_evidence;

#[cfg(target_os = "linux")]
mod mount_execution;
