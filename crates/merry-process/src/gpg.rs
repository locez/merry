use crate::TokioProcessRunner;
use merry_runtime::{
    ProcessActionIntent, ProcessEnvPolicy, ProcessRunner, ProcessRunnerContext, ProcessRunnerError,
};
use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
    time::Duration,
};

/// Validated GnuPG home and socket paths, discovered without starting an agent.
///
/// Only the native socket is granted by the GPG integration. Other sockets are
/// tracked for isolation; SSH authentication requires its separate capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpgAgentSockets {
    home: PathBuf,
    agent: PathBuf,
    auxiliary: Vec<PathBuf>,
}

impl GpgAgentSockets {
    /// Validates explicit GnuPG home and native-agent paths without performing IO.
    pub fn new(
        home: impl Into<PathBuf>,
        agent: impl Into<PathBuf>,
    ) -> Result<Self, ProcessRunnerError> {
        let home = home.into();
        let agent = agent.into();
        validate(&home)?;
        validate(&agent)?;
        Ok(Self {
            home,
            agent,
            auxiliary: Vec::new(),
        })
    }

    /// Adds SSH, extra, or browser sockets that must not be implicitly exposed.
    pub fn with_auxiliary_sockets(
        mut self,
        sockets: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self, ProcessRunnerError> {
        for socket in sockets {
            validate(&socket)?;
            self.auxiliary.push(socket);
        }
        self.auxiliary.sort();
        self.auxiliary.dedup();
        Ok(self)
    }

    /// Returns the home used by GnuPG clients to locate their configuration.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Returns the native GPG agent endpoint, not the SSH protocol endpoint.
    #[must_use]
    pub fn agent(&self) -> &Path {
        &self.agent
    }

    /// Returns the conventional public-key stores imported read-only by GPG integration.
    ///
    /// Private keys, host trust state, configuration, and backup files are not included.
    pub fn public_keyrings(&self) -> impl Iterator<Item = PathBuf> + '_ {
        ["pubring.kbx", "pubring.gpg"]
            .into_iter()
            .map(|name| self.home.join(name))
    }

    /// Rejects keyboxd-backed storage rather than exposing its writable host IPC interface
    /// or presenting an empty or stale file-based keyring as the user's public keys.
    pub fn validate_public_key_access(&self) -> Result<(), ProcessRunnerError> {
        match std::fs::metadata(self.home.join("public-keys.d/pubring.db")) {
            Ok(_) => Err(ProcessRunnerError::infrastructure(
                "gpg_agent public-key integration currently supports pubring.kbx and pubring.gpg; a keyboxd database was detected and its host write interface is not automatically authorized",
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ProcessRunnerError::infrastructure(format!(
                "cannot inspect GPG public-key storage: {error}"
            ))),
        }
    }

    /// Enumerates native and auxiliary endpoints for sandbox exclusion.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.agent.as_path()).chain(self.auxiliary.iter().map(PathBuf::as_path))
    }

    /// Runs `gpgconf --list-dirs` with bounded output and a two-second timeout.
    ///
    /// Returns `None` if gpgconf is not installed. Does not read keys or start an
    /// agent; gpgconf may initialize its runtime socket directory. Invalid output
    /// and discovery failures are errors.
    pub async fn discover(
        home: &Path,
        overrides: &[(OsString, OsString)],
    ) -> Result<Option<Self>, ProcessRunnerError> {
        let search_path = overrides
            .iter()
            .rev()
            .find(|(name, _)| name == "PATH")
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        let mut program = None;
        for directory in std::env::split_paths(&search_path).filter(|path| path.is_absolute()) {
            let candidate = directory.join("gpgconf");
            if tokio::fs::metadata(&candidate)
                .await
                .is_ok_and(|metadata| metadata.is_file())
            {
                program = Some(candidate);
                break;
            }
        }
        let Some(program) = program else {
            return Ok(None);
        };
        let mut environment = overrides.to_vec();
        if !environment.iter().any(|(name, _)| name == "HOME") {
            environment.push((OsString::from("HOME"), home.as_os_str().to_owned()));
        }
        let runner = TokioProcessRunner::new().with_environment_overrides(environment)?;
        let program = program.to_str().ok_or_else(|| {
            ProcessRunnerError::infrastructure("gpgconf executable path is not UTF-8")
        })?;
        let intent = ProcessActionIntent::new(
            vec![program.to_owned(), "--list-dirs".to_owned()],
            None,
            ProcessEnvPolicy::empty(),
            None,
            16 * 1024,
            1024,
        )
        .map_err(|error| {
            ProcessRunnerError::infrastructure(format!("invalid GPG discovery command: {error}"))
        })?;
        let context = ProcessRunnerContext::new(Default::default());
        let cancellation = context.cancellation_token().clone();
        let output = runner.run(intent, context);
        tokio::pin!(output);
        let output = tokio::select! {
            result = &mut output => result?,
            () = tokio::time::sleep(Duration::from_secs(2)) => {
                cancellation.cancel();
                let _ = output.await;
                return Err(ProcessRunnerError::infrastructure("gpgconf socket discovery timed out after two seconds"));
            }
        };
        if !output.ok() || output.stdout_truncated() || !output.stdout_is_utf8() {
            return Err(ProcessRunnerError::infrastructure(
                "gpgconf socket discovery failed or returned invalid/oversized output",
            ));
        }
        Self::parse(output.stdout_text()).map(Some)
    }

    fn parse(output: &str) -> Result<Self, ProcessRunnerError> {
        let mut home = None;
        let mut agent = None;
        let mut auxiliary = Vec::new();
        for line in output.lines() {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            match name {
                "homedir" => home = Some(decode(value)?),
                "agent-socket" => agent = Some(decode(value)?),
                "agent-ssh-socket"
                | "agent-extra-socket"
                | "agent-browser-socket"
                | "keyboxd-socket" => auxiliary.push(decode(value)?),
                _ => {}
            }
        }
        let missing = || {
            ProcessRunnerError::infrastructure("gpgconf did not report homedir and agent-socket")
        };
        Self::new(home.ok_or_else(missing)?, agent.ok_or_else(missing)?)?
            .with_auxiliary_sockets(auxiliary)
    }
}

fn validate(path: &Path) -> Result<(), ProcessRunnerError> {
    if !path.is_absolute()
        || path == Path::new("/")
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        || path
            .to_str()
            .is_none_or(|value| value.chars().any(char::is_control))
    {
        return Err(ProcessRunnerError::infrastructure(
            "GPG socket and home paths must be clean absolute UTF-8 paths",
        ));
    }
    Ok(())
}

fn decode(value: &str) -> Result<PathBuf, ProcessRunnerError> {
    let mut bytes = value.bytes();
    let mut decoded = Vec::new();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let digits = [bytes.next(), bytes.next()];
            let [Some(high), Some(low)] = digits else {
                return Err(ProcessRunnerError::infrastructure(
                    "invalid gpgconf path escape",
                ));
            };
            let high = char::from(high).to_digit(16);
            let low = char::from(low).to_digit(16);
            let (Some(high), Some(low)) = (high, low) else {
                return Err(ProcessRunnerError::infrastructure(
                    "invalid gpgconf path escape",
                ));
            };
            decoded
                .push(u8::try_from(high * 16 + low).map_err(|_| {
                    ProcessRunnerError::infrastructure("invalid gpgconf path byte")
                })?);
        } else {
            decoded.push(byte);
        }
    }
    let path = PathBuf::from(
        String::from_utf8(decoded)
            .map_err(|_| ProcessRunnerError::infrastructure("gpgconf path is not UTF-8"))?,
    );
    validate(&path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_native_and_auxiliary_endpoints_without_conflating_protocols() {
        let sockets = GpgAgentSockets::parse("homedir:/home/user/custom%3ahome\nagent-socket:/run/agent/native\nagent-ssh-socket:/run/agent/ssh\nagent-extra-socket:/run/agent/extra\nagent-browser-socket:/run/agent/browser\nkeyboxd-socket:/run/agent/keyboxd\n").unwrap();
        assert_eq!(sockets.home(), Path::new("/home/user/custom:home"));
        assert_eq!(sockets.agent(), Path::new("/run/agent/native"));
        assert_eq!(sockets.paths().count(), 5);
        assert!(
            sockets
                .paths()
                .any(|path| path == Path::new("/run/agent/keyboxd"))
        );
    }

    #[test]
    fn rejects_incomplete_or_unsafe_socket_discovery() {
        for output in [
            "homedir:/home/user",
            "homedir:/home/user\nagent-socket:relative",
            "homedir:/home/user\nagent-socket:/run/%00socket",
            "homedir:/home/user\nagent-socket:/run/%GGsocket",
            "homedir:/home/user\nagent-socket:/run/../socket",
        ] {
            assert!(GpgAgentSockets::parse(output).is_err());
        }
    }

    #[cfg(unix)]
    fn discovery_fixture(script: &str) -> (tempfile::TempDir, Vec<(OsString, OsString)>) {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let program = fixture.path().join("gpgconf");
        std::fs::write(&program, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let environment = vec![
            (
                OsString::from("PATH"),
                fixture.path().as_os_str().to_owned(),
            ),
            (
                OsString::from("GNUPGHOME"),
                OsString::from("/custom/keyring"),
            ),
        ];
        (fixture, environment)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discovery_uses_the_configured_home_and_never_launches_an_agent() {
        let (fixture, environment) = discovery_fixture(
            "test \"$#\" = 1 && test \"$1\" = --list-dirs && test \"$HOME\" = /fixture/home || exit 1\nprintf 'homedir:%s\\nagent-socket:/custom/runtime/native\\n' \"$GNUPGHOME\"",
        );
        let sockets = GpgAgentSockets::discover(Path::new("/fixture/home"), &environment)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(sockets.home(), Path::new("/custom/keyring"));
        assert_eq!(sockets.agent(), Path::new("/custom/runtime/native"));
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 1);
        std::fs::remove_file(fixture.path().join("gpgconf")).unwrap();
        assert!(
            GpgAgentSockets::discover(Path::new("/fixture/home"), &environment)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discovery_rejects_failure_and_oversized_output_without_reporting_output() {
        for script in [
            "printf 'private diagnostic' >&2; exit 1",
            "printf '%17000s' oversized",
        ] {
            let (_fixture, environment) = discovery_fixture(script);
            let error = GpgAgentSockets::discover(Path::new("/fixture/home"), &environment)
                .await
                .unwrap_err();
            assert!(!error.to_string().contains("private diagnostic"));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discovery_times_out_and_cancels_the_process() {
        let (_fixture, environment) = discovery_fixture("exec /bin/sleep 30");
        let error = GpgAgentSockets::discover(Path::new("/fixture/home"), &environment)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }
}
