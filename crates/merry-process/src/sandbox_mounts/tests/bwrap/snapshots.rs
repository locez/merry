use super::*;
use crate::BwrapSshConfigFiles;
use std::os::unix::fs::MetadataExt;

#[test]
fn ssh_snapshots_replace_the_resolved_target_once_without_flattening_links() {
    let source = Path::new("/etc/ssh/ssh_config");
    let original = fs::read(source).unwrap();
    assert_eq!(
        fs::metadata(source).unwrap().uid(),
        0,
        "test requires a root-owned OpenSSH client configuration"
    );
    for relative in [false, true] {
        let fixture = tempfile::tempdir().unwrap();
        let config = fixture.path().join("ssh/ssh_config.d");
        fs::create_dir_all(&config).unwrap();
        let target = if relative {
            "../../../target"
        } else {
            "/target"
        };
        symlink(target, config.join("20-systemd-ssh-proxy.conf")).unwrap();
        symlink(target, config.join("second.conf")).unwrap();
        fs::write(
            fixture.path().join("ssh/ssh_config"),
            "Include /etc/ssh/ssh_config.d/*.conf\n",
        )
        .unwrap();
        let mut plan = system_plan();
        plan.bind(
            fixture.path(),
            Path::new("/etc"),
            PathAccess::ReadOnly,
            false,
        )
        .unwrap();
        plan.bind(source, Path::new("/target"), PathAccess::ReadOnly, false)
            .unwrap();
        let plan = plan.complete(&["/etc".into()]).unwrap();
        let files = BwrapSshConfigFiles::prepare(Path::new("/etc/ssh/ssh_config"), |path| {
            plan.resolve(path).map_err(|error| {
                merry_runtime::ProcessRunnerError::infrastructure(error.to_string())
            })
        })
        .unwrap();
        assert!(files.compatibility_failure().is_none());
        assert!(files.replaces_file(Path::new("/target")));
        assert!(files.replaces_file(Path::new("/etc/ssh/ssh_config.d/20-systemd-ssh-proxy.conf")));
        let mut args = [
            "--unshare-user",
            "--unshare-pid",
            "--die-with-parent",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
        ]
        .map(OsString::from)
        .to_vec();
        plan.append_args(&mut args, |path| files.replaces_file(path))
            .unwrap();
        files.append_args(&mut args);
        assert_eq!(
            args.windows(3)
                .filter(|args| args[0] == "--ro-bind-data" && args[2] == "/target")
                .count(),
            1
        );
        let mut command = Command::new("bwrap");
        command.args(args).args(["--", "/bin/sh", "-eu", "-c", "test -L /etc/ssh/ssh_config.d/20-systemd-ssh-proxy.conf; test -L /etc/ssh/ssh_config.d/second.conf; test ! -w /target; cat /etc/ssh/ssh_config.d/20-systemd-ssh-proxy.conf"]);
        files.configure_command(&mut command).unwrap();
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, original);
    }
    assert_eq!(fs::read(source).unwrap(), original);
    assert_eq!(fs::metadata(source).unwrap().uid(), 0);
}
