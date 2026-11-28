use super::home;
use crate::config::{MerryConfig, XdgPaths};
use merry::permissions::{PathAccess, PathAccessRuleSource};
use std::path::{Path, PathBuf};

#[test]
fn network_option_defaults_to_open_and_preserves_false() {
    let paths = XdgPaths::from_parts(home(), None, None);
    for (text, allowed) in [
        ("", true),
        ("[permissions]\n", true),
        ("[permissions]\nnetwork = true\n", true),
        ("[permissions]\nnetwork = false\n", false),
    ] {
        let config = MerryConfig::load_optional_from_text(Some(text), &paths)
            .unwrap()
            .unwrap();
        assert_eq!(config.network_requests_allowed(), allowed, "{text}");
    }
    for value in ["0", "\"false\"", "[]"] {
        let text = format!("[permissions]\nnetwork = {value}\n");
        assert!(MerryConfig::load_optional_from_text(Some(&text), &paths).is_err());
    }
}

#[test]
fn review_paths_are_rejected() {
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
    let message = config
        .trusted_global_path_rules()
        .expect_err("review paths")
        .to_string();
    assert!(
        message.contains("review_paths (/abc/d, /shared/private)"),
        "{message}"
    );
    assert!(
        message.contains("readonly_paths or readwrite_paths"),
        "{message}"
    );
    assert!(!message.contains("readonly_paths (/abc)"), "{message}");
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
fn every_legacy_path_key_is_named_when_rejected() {
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

    let message = config
        .trusted_global_path_rules()
        .expect_err("removed path keys")
        .to_string();
    assert!(message.contains("deny_paths (~/.ssh)"), "{message}");
    assert!(message.contains("paths (/var/log/foo ro)"), "{message}");
    assert!(!message.contains("readonly_paths (/etc"), "{message}");
}

#[test]
fn readonly_and_readwrite_paths_stay_preauthorized() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[permissions]
readonly_paths = ["/etc", "~/logs", "shared-readonly"]
readwrite_paths = ["../foo"]
"#,
        ),
        &paths,
    )
    .expect("config should parse")
    .expect("config should be present");

    let rules = config
        .trusted_global_path_rules()
        .expect("read and write paths should resolve");
    assert_eq!(rules.len(), 4);
    assert_eq!(rules[0].path(), Path::new("/etc"));
    assert_eq!(rules[0].access(), PathAccess::ReadOnly);
    assert_eq!(rules[1].path(), Path::new("/home/alice/logs"));
    assert_eq!(
        rules[2].path(),
        Path::new("/home/alice/.config/merry/shared-readonly")
    );
    assert_eq!(rules[3].path(), Path::new("/home/alice/.config/foo"));
    assert_eq!(rules[3].access(), PathAccess::ReadWrite);
    assert!(
        rules
            .iter()
            .all(|rule| rule.source() == PathAccessRuleSource::TrustedGlobalConfig)
    );
}

#[test]
fn rejects_removed_no_sandbox_review_key() {
    let paths = XdgPaths::from_parts(PathBuf::from("/home/alice"), None, None);
    let error = MerryConfig::load_optional_from_text(
        Some("[permissions]\nno_sandbox_review = \"model\"\n"),
        &paths,
    )
    .expect_err("no_sandbox_review was replaced by [cli] approval_policy");
    assert!(error.to_string().contains("no_sandbox_review"), "{error}");
}

#[test]
fn parses_host_integrations_for_outer_ceiling_and_inner_preauthorization() {
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
            merry::permissions::HostIntegration::SshAgent,
            merry::permissions::HostIntegration::SessionBus,
            merry::permissions::HostIntegration::GpgAgent,
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

#[test]
fn parses_and_validates_global_permission_review_rules() {
    let paths = XdgPaths::from_parts(home(), None, None);
    let config = MerryConfig::load_optional_from_text(
        Some(
            r#"
[permissions.review]
environment = ["Internal mirror: https://packages.example.test"]
allow = ["Running local scripts under scripts/dev/"]
soft_deny = ["Modifying database migrations"]
hard_deny = ["Running against production kubeconfig contexts"]
"#,
        ),
        &paths,
    )
    .expect("global review rules should parse")
    .expect("config should be present");

    let rules = config
        .permission_review_rules()
        .expect("review rules should validate");
    assert_eq!(
        rules.environment(),
        ["Internal mirror: https://packages.example.test"]
    );
    assert_eq!(rules.allow(), ["Running local scripts under scripts/dev/"]);
    assert_eq!(rules.soft_deny(), ["Modifying database migrations"]);
    assert_eq!(
        rules.hard_deny(),
        ["Running against production kubeconfig contexts"]
    );

    let blank = MerryConfig::load_optional_from_text(
        Some("[permissions.review]\nallow = [\"   \"]\n"),
        &paths,
    )
    .expect("toml should parse")
    .expect("config should be present");
    assert!(blank.permission_review_rules().is_err());

    let unknown = MerryConfig::load_optional_from_text(
        Some("[permissions.review]\nunsupported = [\"x\"]\n"),
        &paths,
    );
    assert!(unknown.is_err());
}
