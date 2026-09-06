use crate::sandbox::{
    SANDBOX_HOME, SANDBOX_HOME_ROOT, SANDBOX_TMPDIR,
    host::{RuntimeProfile, runtime_profile_from_evidence},
};
use std::ffi::OsStr;

#[test]
fn runtime_profile_requires_tmpfs_home_tmp_and_expected_env() {
    let mountinfo = "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /home rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
";

    assert_eq!(
        runtime_profile_from_evidence(
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        Some(RuntimeProfile::CliBwrap)
    );
    assert_eq!(
        runtime_profile_from_evidence(
            Some(OsStr::new("/home/locez")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        Some(RuntimeProfile::CliBwrap)
    );

    let custom_home_mountinfo = "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /root rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
";
    assert_eq!(
        runtime_profile_from_evidence(
            Some(OsStr::new("/root")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(custom_home_mountinfo),
        ),
        Some(RuntimeProfile::CliBwrap)
    );

    for (home, tmpdir, mountinfo) in [
        (
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new("/var/tmp")),
            Some(mountinfo),
        ),
        (
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(
                "\
26 24 0:22 / / rw,relatime - overlay overlay rw
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
",
            ),
        ),
        (
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(
                "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /home rw,relatime - ext4 /dev/sda1 rw
28 26 0:34 / /tmp rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
",
            ),
        ),
        (
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(
                "\
26 24 0:22 / / rw,relatime - overlay overlay rw
27 26 0:33 / /home rw,nosuid,nodev - tmpfs tmpfs rw,size=65536k
",
            ),
        ),
        (
            Some(OsStr::new(SANDBOX_HOME)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            None,
        ),
        (
            Some(OsStr::new("/")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        (
            Some(OsStr::new(SANDBOX_HOME_ROOT)),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        (
            Some(OsStr::new("/tmp/merry-home")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        (
            Some(OsStr::new("/home/../root")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
        (
            Some(OsStr::new("home/alice")),
            Some(OsStr::new(SANDBOX_TMPDIR)),
            Some(mountinfo),
        ),
    ] {
        assert_eq!(runtime_profile_from_evidence(home, tmpdir, mountinfo), None);
    }
}
