use super::{BwrapSshConfigFiles, includes};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

#[test]
fn include_dependencies_use_ssh_quoting_and_system_relative_paths() {
    let paths = includes::paths(
        "Host *\n  Include=\"ssh_config.d/*.conf\" '/opt/ssh configs/*.conf' # note\nMatch exec \"do-not-execute\"\n Include nested\\ file.conf\n",
        Path::new("/etc/ssh"),
    );
    assert_eq!(
        paths,
        [
            Path::new("/etc/ssh/ssh_config.d/*.conf"),
            Path::new("/opt/ssh configs/*.conf"),
            Path::new("/etc/ssh/nested file.conf"),
        ]
    );
}

#[test]
fn include_globs_cannot_enumerate_a_hidden_directory() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("hidden.conf"), "Host *\n").unwrap();
    let expanded = includes::expand(&directory.path().join("*.conf"), &|_| Ok(None)).unwrap();
    assert!(expanded.is_empty());
}

#[test]
fn unsupported_include_globs_are_left_for_ssh_to_interpret() {
    let directory = tempfile::tempdir().unwrap();
    let expanded = includes::expand(&directory.path().join("["), &|path| {
        Ok(Some(path.to_path_buf()))
    })
    .unwrap();
    assert!(expanded.is_empty());
}

#[test]
fn insecure_config_is_not_parsed_or_repaired() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("ssh_config");
    fs::write(&config, "Include must-not-be-read\n").unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o666)).unwrap();
    let files = BwrapSshConfigFiles::prepare(&config, |path| {
        assert_eq!(path, config);
        Ok(Some(path.to_path_buf()))
    })
    .unwrap();
    let mut command = std::process::Command::new("true");
    files.configure_command(&mut command).unwrap();
    assert!(command.status().unwrap().success());
    assert_eq!(
        fs::metadata(config).unwrap().permissions().mode() & 0o777,
        0o666
    );
}

#[test]
fn oversized_config_leaves_original_mounts_without_blocking_unrelated_actions() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("ssh_config");
    fs::write(&config, vec![b'x'; 1024 * 1024 + 1]).unwrap();
    let files = BwrapSshConfigFiles::prepare(&config, |path| Ok(Some(path.to_path_buf()))).unwrap();
    let error = files
        .compatibility_failure()
        .expect("bounded compatibility failure");
    assert!(error.to_string().contains("exceeds 1 MiB"), "{error}");
    let mut args = Vec::new();
    files.append_args(&mut args);
    assert!(args.is_empty());
    let mut command = std::process::Command::new("true");
    files.configure_command(&mut command).unwrap();
    assert!(command.status().unwrap().success());
}

#[test]
fn namespace_policy_errors_are_not_compatibility_fallbacks() {
    let error = BwrapSshConfigFiles::prepare(Path::new("/etc/ssh/ssh_config"), |_| {
        Err(merry_runtime::ProcessRunnerError::infrastructure(
            "policy resolver rejected path",
        ))
    })
    .unwrap_err();
    assert!(error.to_string().contains("policy resolver rejected path"));
}
