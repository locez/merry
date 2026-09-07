use super::home;
use crate::config::{MerryConfig, XdgPaths};
use merry::profiles::NoSandboxReviewMode;
use merry_runtime::{PathAccess, PathAccessRuleSource};
use std::path::{Path, PathBuf};

#[test]
fn review_paths_restrict_declared_subtrees_without_changing_preauthorization() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[permissions]
readonly_paths = ["/abc"]
readwrite_paths = ["/shared"]
review_paths = ["/abc/d", "/shared/private"]
"#,
        ),
        &paths,
    )
    .unwrap()
    .unwrap();
    let rules = config.trusted_global_path_rules().unwrap();
    assert_eq!(rules.len(), 4);
    assert!(!rules[0].review_required());
    assert!(!rules[1].review_required());
    assert_eq!(rules[1].access(), PathAccess::ReadWrite);
    assert_eq!(rules[2].path(), Path::new("/abc/d"));
    assert_eq!(rules[2].access(), PathAccess::ReadOnly);
    assert!(rules[2].review_required());
    assert_eq!(rules[3].access(), PathAccess::ReadWrite);
    assert!(rules[3].review_required());
}

#[test]
fn review_paths_cannot_create_grants_or_override_denial() {
    let paths = XdgPaths::from_parts(home(), None, None);
    for input in [
        "readonly_paths = ['/abc']\nreview_paths = ['/elsewhere']",
        "readonly_paths = ['/abc']\ndeny_paths = ['/abc/d']\nreview_paths = ['/abc/d/secret']",
    ] {
        let config =
            MerryConfig::load_optional_from_text(Some(&format!("[permissions]\n{input}")), &paths)
                .unwrap()
                .unwrap();
        assert!(config.trusted_global_path_rules().is_err());
    }
}

#[test]
fn parses_trusted_global_path_rules() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[permissions]
readonly_paths = ["/etc", "~/logs", "shared-readonly"]
readwrite_paths = ["../foo"]
deny_paths = ["~/.ssh"]

[[permissions.paths]]
path = "/var/log/foo"
access = "ro"
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let rules = config
        .trusted_global_path_rules()
        .expect("trusted path rules should resolve");
    assert_eq!(rules.len(), 6);
    assert_eq!(rules[0].path(), Path::new("/etc"));
    assert_eq!(rules[0].access(), PathAccess::ReadOnly);
    assert_eq!(rules[1].path(), Path::new("/home/alice/logs"));
    assert_eq!(
        rules[2].path(),
        Path::new("/home/alice/.config/merry/shared-readonly")
    );
    assert_eq!(rules[3].path(), Path::new("/home/alice/.config/foo"));
    assert_eq!(rules[3].access(), PathAccess::ReadWrite);
    assert_eq!(rules[4].path(), Path::new("/home/alice/.ssh"));
    assert_eq!(rules[4].access(), PathAccess::Deny);
    assert_eq!(rules[5].path(), Path::new("/var/log/foo"));
    assert_eq!(rules[5].access(), PathAccess::ReadOnly);
    assert!(
        rules
            .iter()
            .all(|rule| rule.source() == PathAccessRuleSource::TrustedGlobalConfig)
    );
}

#[test]
fn configures_model_review_for_no_sandbox_mode() {
    let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
    let model = MerryConfig::load_optional_from_text(
        Some("[permissions]\nno_sandbox_review = \"model\"\n"),
        &paths,
    )
    .expect("permission review config should parse")
    .expect("permission review config should exist");
    assert_eq!(model.no_sandbox_review_mode(), NoSandboxReviewMode::Model);

    let default = MerryConfig::load_optional_from_text(
        Some("[permissions]\nno_sandbox_review = \"host\"\n"),
        &paths,
    )
    .expect("host review config should parse")
    .expect("host review config should exist");
    assert_eq!(default.no_sandbox_review_mode(), NoSandboxReviewMode::Host);
}

#[test]
fn parses_host_integrations_for_outer_sandbox_ceiling() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[permissions]
ssh_agent = true
dbus = true
gpg_agent = true
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    assert_eq!(
        config.host_integrations(),
        vec![
            merry_runtime::HostIntegration::SshAgent,
            merry_runtime::HostIntegration::SessionBus,
            merry_runtime::HostIntegration::GpgAgent,
        ]
    );
}

#[test]
fn rejects_legacy_session_bus_configuration_name() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let error = MerryConfig::load_optional_from_text(
        Some(
            r#"
[permissions]
session_bus = true
"#,
        ),
        &paths,
    )
    .expect_err("unpublished legacy configuration name must be rejected");
    assert!(error.to_string().contains("unknown field `session_bus`"));
}

#[test]
fn parses_and_validates_process_environment_overrides() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[permissions]
environment = [
  { name = "RUSTUP_TOOLCHAIN", value = "stable" },
  { name = "CARGO_TERM_COLOR", value = "always" },
]
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    assert_eq!(
        config
            .process_environment_overrides()
            .expect("environment overrides should validate"),
        vec![
            ("RUSTUP_TOOLCHAIN".to_owned(), "stable".to_owned()),
            ("CARGO_TERM_COLOR".to_owned(), "always".to_owned()),
        ]
    );

    for invalid in ["", "1INVALID", "INVALID-NAME", "INVALID=NAME"] {
        let text =
            format!("[permissions]\nenvironment = [{{ name = {invalid:?}, value = \"x\" }}]");
        let config =
            MerryConfig::load_optional_from_text(Some(&text), &paths).expect("config should parse");
        let config = config.expect("config should be present");
        assert!(
            config.process_environment_overrides().is_err(),
            "{invalid:?} should be rejected"
        );
    }

    let duplicate = MerryConfig::load_optional_from_text(
        Some(
            r#"
[permissions]
environment = [
  { name = "DUPLICATE", value = "one" },
  { name = "DUPLICATE", value = "two" },
]
"#,
        ),
        &paths,
    )
    .expect("duplicate environment config should parse")
    .expect("duplicate environment config should be present");
    assert!(duplicate.process_environment_overrides().is_err());

    let nul_value = MerryConfig::load_optional_from_text(
        Some("[permissions]\nenvironment = [{ name = \"NUL_VALUE\", value = \"\\u0000\" } ]"),
        &paths,
    )
    .expect("NUL environment config should parse")
    .expect("NUL environment config should be present");
    assert!(nul_value.process_environment_overrides().is_err());
}

#[test]
fn rejects_unknown_path_access() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let error = MerryConfig::load_optional_from_text(
        Some(
            r#"
[[permissions.paths]]
path = "/etc"
access = "write"
"#,
        ),
        &paths,
    )
    .expect_err("unknown path access should fail parsing");

    assert!(error.to_string().contains("access"));
}
