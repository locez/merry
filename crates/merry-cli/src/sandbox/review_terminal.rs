//! Hands the controlling terminal to a sandboxed `merry run -` for permission
//! review answers.
//!
//! The outer bubblewrap sandbox starts its child with `--new-session`, which
//! detaches it from the controlling terminal so a sandboxed process cannot
//! inject keystrokes into it. That also makes `/dev/tty` unopenable inside
//! the sandbox (`ENXIO`). A `-` task consumes stdin before the runtime
//! starts, so a sandboxed run that may ask a person for approval would have
//! nowhere left to read the answer and would deny every request unanswered.
//!
//! The parent therefore resolves its controlling terminal's device node
//! before the re-exec and binds only that node into the sandbox at
//! [`SANDBOX_REVIEW_TERMINAL_PATH`]. Opening a terminal device by path works
//! without a controlling terminal, while `--new-session` keeps its effect:
//! the device is not the child's controlling terminal, so job control and
//! `TIOCSTI` stay unavailable to it. Inner action sandboxes mount their own
//! `/dev`, so the node never reaches the processes a run executes.

use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
};

/// Path inside the outer sandbox where the parent's terminal device is bound.
pub(crate) const SANDBOX_REVIEW_TERMINAL_PATH: &str = "/dev/merry-review-tty";

/// Directories searched for the controlling terminal's device node.
#[cfg(target_os = "linux")]
const TERMINAL_DEVICE_DIRS: [&str; 2] = ["/dev/pts", "/dev"];

/// The controlling terminal's device node, resolved before the re-exec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewTerminalHandoff {
    device: PathBuf,
}

impl ReviewTerminalHandoff {
    /// Resolves the current process's controlling terminal; `None` when the
    /// process has none or its device node cannot be found.
    #[cfg(target_os = "linux")]
    pub(crate) fn resolve_controlling_terminal() -> Option<Self> {
        let stat = fs::read_to_string("/proc/self/stat").ok()?;
        let device_number = controlling_terminal_number(&stat)?;
        let device = find_terminal_device(device_number, &TERMINAL_DEVICE_DIRS)?;
        Some(Self { device })
    }

    /// The outer bubblewrap sandbox, and with it this handoff, exists only
    /// on Linux; elsewhere there is never a terminal to bind.
    #[cfg(not(target_os = "linux"))]
    pub(crate) fn resolve_controlling_terminal() -> Option<Self> {
        None
    }

    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn for_device(device: PathBuf) -> Self {
        Self { device }
    }

    /// Host path of the terminal device node to bind into the sandbox.
    pub(crate) fn device(&self) -> &Path {
        &self.device
    }
}

/// Extracts `tty_nr` from `/proc/self/stat`; `None` when the process has no
/// controlling terminal.
///
/// The command name in field two may contain spaces and parentheses, so
/// fields are counted from the last closing parenthesis.
#[cfg(target_os = "linux")]
fn controlling_terminal_number(stat: &str) -> Option<u64> {
    let (_, after_command) = stat.rsplit_once(')')?;
    // After the command: state ppid pgrp session tty_nr ...
    let tty_nr = after_command
        .split_ascii_whitespace()
        .nth(4)?
        .parse()
        .ok()?;
    (tty_nr != 0).then_some(tty_nr)
}

/// Finds the character device in `dirs` whose device number is
/// `device_number`, searching each directory's direct children only.
#[cfg(target_os = "linux")]
fn find_terminal_device(device_number: u64, dirs: &[&str]) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
            let path = entry.path();
            let metadata = fs::metadata(&path).ok()?;
            (metadata.file_type().is_char_device() && metadata.rdev() == device_number)
                .then_some(path)
        })
    })
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) mod tests {
    use super::{
        ReviewTerminalHandoff, SANDBOX_REVIEW_TERMINAL_PATH, controlling_terminal_number,
        find_terminal_device,
    };
    use std::{
        fs::File,
        io::Write,
        os::unix::fs::MetadataExt,
        path::PathBuf,
        process::{Command, Stdio},
    };

    /// A pseudo-terminal pair: the master end and the slave's device path.
    pub(crate) struct PseudoTerminal {
        pub(crate) master: File,
        pub(crate) slave_path: PathBuf,
    }

    /// Allocates a pseudo-terminal the way a terminal emulator does.
    pub(crate) fn open_pseudo_terminal() -> PseudoTerminal {
        let master = rustix::pty::openpt(
            rustix::pty::OpenptFlags::RDWR
                | rustix::pty::OpenptFlags::NOCTTY
                | rustix::pty::OpenptFlags::CLOEXEC,
        )
        .expect("pseudo-terminal master");
        rustix::pty::unlockpt(&master).expect("unlock pseudo-terminal slave");
        let slave_path = rustix::pty::ptsname(&master, Vec::new())
            .expect("pseudo-terminal slave name")
            .into_string()
            .expect("slave path should be UTF-8");
        PseudoTerminal {
            master: File::from(master),
            slave_path: PathBuf::from(slave_path),
        }
    }

    #[test]
    fn controlling_terminal_number_is_read_past_an_awkward_command_name() {
        let stat = "4242 (merry (run) -) S 1 4242 4242 34828 4242 4194560 0 0\n";
        assert_eq!(controlling_terminal_number(stat), Some(34_828));

        let no_terminal = "4242 (merry) S 1 4242 4242 0 -1 4194560 0 0\n";
        assert_eq!(controlling_terminal_number(no_terminal), None);
        assert_eq!(controlling_terminal_number("garbage"), None);
    }

    #[test]
    fn terminal_device_is_found_by_its_device_number() {
        let terminal = open_pseudo_terminal();
        let device_number = std::fs::metadata(&terminal.slave_path)
            .expect("slave metadata")
            .rdev();

        let found = find_terminal_device(device_number, &["/nonexistent", "/dev/pts"])
            .expect("the slave should be found under /dev/pts");
        assert_eq!(
            std::fs::metadata(&found).expect("found metadata").rdev(),
            device_number
        );
        assert_eq!(found, terminal.slave_path);

        // A regular directory without the device yields nothing rather than a
        // wrong node.
        assert_eq!(find_terminal_device(device_number, &["/etc"]), None);
        assert_eq!(
            ReviewTerminalHandoff::for_device(found.clone()).device(),
            found.as_path()
        );
    }

    /// The real combination the handoff exists for: a `--new-session`
    /// bubblewrap child cannot open `/dev/tty`, yet reads the operator's
    /// answer through the bound terminal device.
    #[test]
    fn bound_terminal_answers_inside_a_new_session_sandbox_without_dev_tty() {
        let mut terminal = open_pseudo_terminal();
        // Like a shell, keep a slave open so typed input is queued for the
        // sandboxed reader even before it opens the device.
        let _slave = File::open(&terminal.slave_path).expect("slave should open");
        let script = format!(
            "if (exec 8</dev/tty) 2>/dev/null; then echo tty=open; else echo tty=closed; fi; \
             printf 'Allow? [y/N] ' >>{path}; read -r answer <{path}; echo \"answer=$answer\"",
            path = SANDBOX_REVIEW_TERMINAL_PATH
        );
        let child = Command::new("bwrap")
            .args([
                "--unshare-user",
                "--unshare-pid",
                "--die-with-parent",
                "--new-session",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--dev-bind",
            ])
            .arg(&terminal.slave_path)
            .args([SANDBOX_REVIEW_TERMINAL_PATH, "sh", "-c", &script])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("bubblewrap for Linux sandbox tests");

        terminal
            .master
            .write_all(b"yes\n")
            .expect("answer should reach the terminal");
        let output = child.wait_with_output().expect("sandbox should exit");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "sandbox failed: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("tty=closed"),
            "--new-session should still detach /dev/tty: {stdout}"
        );
        assert!(
            stdout.contains("answer=yes"),
            "the answer should arrive through the bound device: {stdout}"
        );
    }
}
