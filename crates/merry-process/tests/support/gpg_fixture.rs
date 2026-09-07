use std::{fs, path::Path, process::Command};

/// Generates anonymous public test data without consulting host identities.
/// Private keys and agents live only in the disposable mount/PID namespaces.
/// Only public exports and signatures reach the caller's temporary directory.
pub(crate) fn generate(workspace: &Path) -> String {
    let script = r#"
        mkdir -m 700 "$GNUPGHOME"
        identity='Merry Sandbox Fixture <fixture@example.invalid>'
        printf 'Merry sandbox public-key verification fixture.\n' > /tmp/public/message.txt
        gpg --no-options --batch --pinentry-mode loopback --passphrase '' \
            --quick-generate-key "$identity" ed25519 sign 0
        gpg --no-options --batch --armor --output /tmp/public/public.asc --export "$identity"
        gpg --no-options --batch --with-colons --fingerprint --list-keys "$identity" \
            | awk -F: '$1 == "fpr" { print $10; exit }' > /tmp/public/fingerprint.txt
        fingerprint=$(cat /tmp/public/fingerprint.txt)
        test -n "$fingerprint"
        gpg --no-options --batch --pinentry-mode loopback --passphrase '' \
            --local-user "$fingerprint" --armor --output /tmp/public/message.asc \
            --detach-sign /tmp/public/message.txt
    "#;
    let output = Command::new("timeout")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args([
            "--kill-after=2s",
            "20s",
            "bwrap",
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--tmpfs",
            "/run",
            "--tmpfs",
            "/etc",
            "--ro-bind-try",
            "/etc/ld.so.cache",
            "/etc/ld.so.cache",
            "--tmpfs",
            "/home",
            "--tmpfs",
            "/root",
            "--bind",
        ])
        .arg(workspace)
        .args([
            "/tmp/public",
            "--clearenv",
            "--setenv",
            "PATH",
            "/usr/bin:/bin",
            "--setenv",
            "HOME",
            "/tmp",
            "--setenv",
            "GNUPGHOME",
            "/tmp/fixture-keyring",
            "--",
            "/bin/sh",
            "-eu",
            "-c",
            script,
        ])
        .output()
        .expect("isolated GnuPG fixture dependencies");
    assert!(
        output.status.success(),
        "anonymous fixture generation failed: {output:?}"
    );
    let fingerprint = fs::read_to_string(workspace.join("fingerprint.txt")).unwrap();
    let fingerprint = fingerprint.trim().to_owned();
    assert_eq!(fingerprint.len(), 40);
    assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
    fingerprint
}
