use super::{ConfigError, MerryConfig};
use crate::coding::ProcessExecutionMode;
use serde::Deserialize;
use serde::de::{self, Deserializer, IntoDeserializer, SeqAccess, Visitor};
use std::fmt;

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct CliToml {
    default_options: Option<DefaultOptionsToml>,
}

/// One of the four modes `[cli] default_options` may select.
///
/// `with-sandbox`, `no-sandbox`, and `inner-sandbox` choose the process
/// execution mode that `--with-sandbox`, `--no-sandbox`, and `--inner-sandbox`
/// select on the command line; `fully-trusted` is the `--fully-trusted` review
/// mode.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CliDefaultOption {
    WithSandbox,
    NoSandbox,
    InnerSandbox,
    FullyTrusted,
}

impl CliDefaultOption {
    const fn as_str(self) -> &'static str {
        match self {
            Self::WithSandbox => "with-sandbox",
            Self::NoSandbox => "no-sandbox",
            Self::InnerSandbox => "inner-sandbox",
            Self::FullyTrusted => "fully-trusted",
        }
    }

    /// The process execution mode this option selects, if it is a sandbox mode.
    const fn sandbox_mode(self) -> Option<ProcessExecutionMode> {
        match self {
            Self::WithSandbox => Some(ProcessExecutionMode::OuterAndInner),
            Self::NoSandbox => Some(ProcessExecutionMode::Unrestricted),
            Self::InnerSandbox => Some(ProcessExecutionMode::InnerOnly),
            Self::FullyTrusted => None,
        }
    }
}

impl fmt::Display for CliDefaultOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `default_options` accepts one option name or an array of option names, so
/// choosing a single sandbox mode does not require array syntax.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DefaultOptionsToml(Vec<CliDefaultOption>);

impl<'de> Deserialize<'de> for DefaultOptionsToml {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct OptionsVisitor;

        impl<'de> Visitor<'de> for OptionsVisitor {
            type Value = DefaultOptionsToml;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a default option name or an array of default option names")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                CliDefaultOption::deserialize(value.into_deserializer())
                    .map(|option| DefaultOptionsToml(vec![option]))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut options = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(option) = seq.next_element()? {
                    options.push(option);
                }
                Ok(DefaultOptionsToml(options))
            }
        }

        deserializer.deserialize_any(OptionsVisitor)
    }
}

/// Resolved `[cli] default_options`: the configured process execution mode,
/// if any, and whether fully trusted review is on by default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CliDefaultOptions {
    process_execution_mode: Option<ProcessExecutionMode>,
    fully_trusted: bool,
}

impl CliDefaultOptions {
    /// Maps the listed options to modes.
    ///
    /// At most one sandbox mode may be listed and no option may repeat,
    /// mirroring clap's rules for the corresponding flags.
    pub(crate) fn from_options(options: &[CliDefaultOption]) -> Result<Self, ConfigError> {
        let mut defaults = Self::default();
        let mut selected_sandbox_option = None;
        let mut seen = Vec::with_capacity(options.len());
        for &option in options {
            if seen.contains(&option) {
                return Err(ConfigError::Invalid(format!(
                    "cli.default_options repeats {option}"
                )));
            }
            seen.push(option);
            match option.sandbox_mode() {
                Some(mode) => {
                    if let Some(conflict) = selected_sandbox_option {
                        return Err(ConfigError::Invalid(format!(
                            "cli.default_options cannot combine {conflict} with {option}; choose one sandbox mode"
                        )));
                    }
                    selected_sandbox_option = Some(option);
                    defaults.process_execution_mode = Some(mode);
                }
                None => defaults.fully_trusted = true,
            }
        }
        Ok(defaults)
    }

    /// The configured sandbox mode, when one of the three is listed.
    pub(crate) const fn process_execution_mode(self) -> Option<ProcessExecutionMode> {
        self.process_execution_mode
    }

    /// Whether `fully-trusted` is listed.
    pub(crate) const fn fully_trusted(self) -> bool {
        self.fully_trusted
    }
}

impl MerryConfig {
    /// Returns the resolved `[cli] default_options`, empty when unset.
    pub(crate) fn cli_default_options(&self) -> Result<CliDefaultOptions, ConfigError> {
        let Some(options) = self
            .raw
            .cli
            .as_ref()
            .and_then(|cli| cli.default_options.as_ref())
        else {
            return Ok(CliDefaultOptions::default());
        };
        CliDefaultOptions::from_options(&options.0)
    }
}
