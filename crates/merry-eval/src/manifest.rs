use crate::error::{
    EvalError, IDENTIFIER_PATTERN, NON_BLANK_TEXT_PATTERN, RELATIVE_PATH_PATTERN, invalid_field,
    optional_relative_path_schema, optional_sha256_schema, relative_path_schema,
    validate_identifier, validate_relative_path, validate_text,
};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de, ser::Error as SerdeError};

use crate::numeric::{
    deserialize_option_u32, deserialize_option_u64, deserialize_u32, deserialize_u64,
    optional_positive_u32_schema, optional_positive_u64_schema, task_timeout_schema,
    version_schema,
};

#[path = "manifest_validation.rs"]
mod validation;
use validation::{validate_repository, validate_scope, validate_sha256, validate_text_field};

/// The only task-manifest schema version understood by this crate.
pub const TASK_SCHEMA_VERSION: u32 = 1;

const MAX_DESCRIPTION_CHARS: usize = 16 * 1024;
const MAX_DIFF_CHARS: usize = 1024 * 1024;
const MAX_COMMAND_ARGS: usize = 256;
const MAX_COMMAND_ARG_CHARS: usize = 16 * 1024;
const MAX_SCOPE_CHARS: usize = 512;
const MAX_PATH_CHARS: usize = 512;

/// A repository or image from which an evaluation task is prepared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(schema_with = "repository_spec_schema")]
#[serde(try_from = "RepositorySpecWire")]
pub struct RepositorySpec {
    /// Relative fixture path, when the task uses a checked-out repository.
    path: Option<String>,
    /// Container or VM image reference, when the task uses an image.
    image: Option<String>,
    /// Optional source revision to check out inside the repository or image.
    commit: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RepositorySpecWire {
    path: Option<String>,
    image: Option<String>,
    commit: Option<String>,
}

impl TryFrom<RepositorySpecWire> for RepositorySpec {
    type Error = EvalError;

    fn try_from(wire: RepositorySpecWire) -> Result<Self, Self::Error> {
        let repository = Self {
            path: wire.path,
            image: wire.image,
            commit: wire.commit,
        };
        validate_repository(&repository)?;
        Ok(repository)
    }
}

impl RepositorySpec {
    /// Creates a repository specification backed by a relative fixture path.
    pub fn from_path(path: &str) -> Result<Self, EvalError> {
        validate_relative_path("repository.path", path)?;
        Ok(Self {
            path: Some(path.to_owned()),
            image: None,
            commit: None,
        })
    }

    /// Creates a repository specification backed by an image reference.
    pub fn from_image(image: &str) -> Result<Self, EvalError> {
        validate_text("repository.image", image, MAX_PATH_CHARS)?;
        Ok(Self {
            path: None,
            image: Some(image.to_owned()),
            commit: None,
        })
    }

    /// Adds a source revision to this repository specification.
    pub fn with_commit(mut self, commit: &str) -> Result<Self, EvalError> {
        validate_identifier("repository.commit", commit, 256)?;
        self.commit = Some(commit.to_owned());
        Ok(self)
    }

    /// Returns the optional relative fixture path.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Returns the optional image reference.
    #[must_use]
    pub fn image(&self) -> Option<&str> {
        self.image.as_deref()
    }

    /// Returns the optional source revision.
    #[must_use]
    pub fn commit(&self) -> Option<&str> {
        self.commit.as_deref()
    }
}

fn repository_spec_schema(generator: &mut SchemaGenerator) -> Schema {
    let path_schema = relative_path_schema(generator);
    let image_schema = json_schema!({
        "type": "string",
        "minLength": 1,
        "maxLength": MAX_PATH_CHARS,
        "pattern": NON_BLANK_TEXT_PATTERN,
    });
    let commit_schema = json_schema!({
        "type": ["string", "null"],
        "minLength": 1,
        "maxLength": 256,
        "pattern": IDENTIFIER_PATTERN,
    });
    json_schema!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["path"],
                "properties": {
                    "path": path_schema,
                    "image": {"type": "null"},
                    "commit": commit_schema,
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "required": ["image"],
                "properties": {
                    "path": {"type": "null"},
                    "image": image_schema,
                    "commit": commit_schema,
                }
            }
        ]
    })
}

/// A typed executable command used by setup and test stages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "CommandSpecWire")]
pub struct CommandSpec {
    /// Executable token; shell interpretation is not implied.
    #[schemars(
        length(min = 1, max = MAX_PATH_CHARS),
        regex(pattern = NON_BLANK_TEXT_PATTERN)
    )]
    program: String,
    /// Explicit arguments passed to the executable.
    #[serde(default)]
    #[schemars(
        length(max = MAX_COMMAND_ARGS),
        inner(
            length(max = MAX_COMMAND_ARG_CHARS),
            regex(pattern = NON_BLANK_TEXT_PATTERN)
        )
    )]
    args: Vec<String>,
    /// Optional relative working directory inside the task workspace.
    #[serde(default)]
    #[schemars(default, schema_with = "optional_relative_path_schema")]
    working_dir: Option<String>,
    /// Optional command-specific timeout.
    #[serde(default, deserialize_with = "deserialize_option_u64")]
    #[schemars(default, schema_with = "optional_positive_u64_schema")]
    timeout_seconds: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct CommandSpecWire {
    #[schemars(
        length(min = 1, max = MAX_PATH_CHARS),
        regex(pattern = NON_BLANK_TEXT_PATTERN)
    )]
    program: String,
    #[serde(default)]
    #[schemars(
        default,
        length(max = MAX_COMMAND_ARGS),
        inner(
            length(max = MAX_COMMAND_ARG_CHARS),
            regex(pattern = NON_BLANK_TEXT_PATTERN)
        )
    )]
    args: Vec<String>,
    #[serde(default)]
    #[schemars(default, schema_with = "optional_relative_path_schema")]
    working_dir: Option<String>,
    #[serde(default, deserialize_with = "deserialize_option_u64")]
    #[schemars(default, schema_with = "optional_positive_u64_schema")]
    timeout_seconds: Option<u64>,
}

impl TryFrom<CommandSpecWire> for CommandSpec {
    type Error = EvalError;

    fn try_from(wire: CommandSpecWire) -> Result<Self, Self::Error> {
        let command = Self {
            program: wire.program,
            args: wire.args,
            working_dir: wire.working_dir,
            timeout_seconds: wire.timeout_seconds,
        };
        command.validate("command")?;
        Ok(command)
    }
}

impl CommandSpec {
    /// Creates a command with explicit executable arguments.
    pub fn new(program: &str, args: &[&str]) -> Result<Self, EvalError> {
        let command = Self {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            working_dir: None,
            timeout_seconds: None,
        };
        command.validate("command")?;
        Ok(command)
    }

    /// Returns the executable token.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Returns explicit command arguments.
    #[must_use]
    pub fn args(&self) -> &[String] {
        &self.args
    }

    /// Returns the optional working directory.
    #[must_use]
    pub fn working_dir(&self) -> Option<&str> {
        self.working_dir.as_deref()
    }

    /// Sets a relative working directory for this command.
    pub fn with_working_dir(mut self, working_dir: &str) -> Result<Self, EvalError> {
        validate_relative_path("command.working_dir", working_dir)?;
        self.working_dir = Some(working_dir.to_owned());
        Ok(self)
    }

    /// Sets a positive command-specific timeout.
    pub fn with_timeout_seconds(mut self, timeout_seconds: u64) -> Result<Self, EvalError> {
        if timeout_seconds == 0 {
            return Err(invalid_field(
                "command.timeout_seconds",
                "must be greater than zero when provided",
            ));
        }
        self.timeout_seconds = Some(timeout_seconds);
        Ok(self)
    }

    /// Returns the optional command timeout.
    #[must_use]
    pub fn timeout_seconds(&self) -> Option<u64> {
        self.timeout_seconds
    }

    fn validate(&self, field: &str) -> Result<(), EvalError> {
        validate_text_field(&format!("{field}.program"), &self.program, MAX_PATH_CHARS)?;
        if self.args.len() > MAX_COMMAND_ARGS {
            return Err(invalid_field(
                format!("{field}.args"),
                format!("must contain at most {MAX_COMMAND_ARGS} arguments"),
            ));
        }
        for (index, arg) in self.args.iter().enumerate() {
            validate_text_field(
                &format!("{field}.args[{index}]"),
                arg,
                MAX_COMMAND_ARG_CHARS,
            )?;
        }
        if let Some(working_dir) = self.working_dir.as_deref() {
            validate_relative_path(&format!("{field}.working_dir"), working_dir)?;
        }
        if self.timeout_seconds == Some(0) {
            return Err(invalid_field(
                format!("{field}.timeout_seconds"),
                "must be greater than zero when provided",
            ));
        }
        Ok(())
    }
}

/// The amount of external authority a task may use while it runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RiskPolicy {
    /// The task may inspect its workspace but must not mutate it.
    ReadOnly,
    /// The task may mutate files inside its declared workspace scope.
    #[default]
    WorkspaceWrite,
    /// The task may mutate its workspace and access the network.
    WorkspaceWriteAndNetwork,
}

/// Resource ceilings applied by an evaluation harness.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "ResourceLimitsWire")]
pub struct ResourceLimits {
    /// Maximum captured process output in bytes.
    #[schemars(default, schema_with = "optional_positive_u64_schema")]
    max_output_bytes: Option<u64>,
    /// Maximum number of files a task may change.
    #[serde(default, deserialize_with = "deserialize_option_u32")]
    #[schemars(default, schema_with = "optional_positive_u32_schema")]
    max_file_changes: Option<u32>,
    /// Maximum number of concurrently running processes.
    #[serde(default, deserialize_with = "deserialize_option_u32")]
    #[schemars(default, schema_with = "optional_positive_u32_schema")]
    max_processes: Option<u32>,
}

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ResourceLimitsWire {
    #[serde(default, deserialize_with = "deserialize_option_u64")]
    #[schemars(schema_with = "optional_positive_u64_schema")]
    max_output_bytes: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_option_u32")]
    #[schemars(schema_with = "optional_positive_u32_schema")]
    max_file_changes: Option<u32>,
    #[serde(default, deserialize_with = "deserialize_option_u32")]
    #[schemars(schema_with = "optional_positive_u32_schema")]
    max_processes: Option<u32>,
}

impl TryFrom<ResourceLimitsWire> for ResourceLimits {
    type Error = EvalError;

    fn try_from(wire: ResourceLimitsWire) -> Result<Self, Self::Error> {
        let limits = Self {
            max_output_bytes: wire.max_output_bytes,
            max_file_changes: wire.max_file_changes,
            max_processes: wire.max_processes,
        };
        limits.validate()?;
        Ok(limits)
    }
}

impl ResourceLimits {
    /// Creates validated resource ceilings for one task.
    pub fn new(
        max_output_bytes: Option<u64>,
        max_file_changes: Option<u32>,
        max_processes: Option<u32>,
    ) -> Result<Self, EvalError> {
        let limits = Self {
            max_output_bytes,
            max_file_changes,
            max_processes,
        };
        limits.validate()?;
        Ok(limits)
    }

    /// Returns the maximum captured output size.
    #[must_use]
    pub fn max_output_bytes(&self) -> Option<u64> {
        self.max_output_bytes
    }

    /// Returns the maximum file-change count.
    #[must_use]
    pub fn max_file_changes(&self) -> Option<u32> {
        self.max_file_changes
    }

    /// Returns the maximum process count.
    #[must_use]
    pub fn max_processes(&self) -> Option<u32> {
        self.max_processes
    }

    fn validate(&self) -> Result<(), EvalError> {
        if self.max_output_bytes == Some(0) {
            return Err(invalid_field(
                "resource_limits.max_output_bytes",
                "must be greater than zero when provided",
            ));
        }
        if self.max_file_changes == Some(0) {
            return Err(invalid_field(
                "resource_limits.max_file_changes",
                "must be greater than zero when provided",
            ));
        }
        if self.max_processes == Some(0) {
            return Err(invalid_field(
                "resource_limits.max_processes",
                "must be greater than zero when provided",
            ));
        }
        Ok(())
    }
}

/// A typed expected artifact emitted by a successful task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "ExpectedArtifactWire")]
pub struct ExpectedArtifact {
    /// Relative path of the expected artifact.
    #[schemars(schema_with = "relative_path_schema")]
    path: String,
    /// Provider-neutral artifact kind.
    kind: ArtifactKind,
    /// Optional SHA-256 digest of the artifact bytes.
    #[schemars(default, schema_with = "optional_sha256_schema")]
    sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExpectedArtifactWire {
    #[schemars(schema_with = "relative_path_schema")]
    path: String,
    kind: ArtifactKind,
    #[schemars(default, schema_with = "optional_sha256_schema")]
    sha256: Option<String>,
}

impl TryFrom<ExpectedArtifactWire> for ExpectedArtifact {
    type Error = EvalError;

    fn try_from(wire: ExpectedArtifactWire) -> Result<Self, Self::Error> {
        Self::from_wire_at(wire, "expected_artifact")
    }
}

impl ExpectedArtifact {
    fn from_wire_at(wire: ExpectedArtifactWire, field: &str) -> Result<Self, EvalError> {
        let artifact = Self {
            path: wire.path,
            kind: wire.kind,
            sha256: wire.sha256,
        };
        artifact.validate_at(field)?;
        Ok(artifact)
    }
    /// Creates a validated expected artifact reference.
    pub fn new(path: &str, kind: ArtifactKind, sha256: Option<&str>) -> Result<Self, EvalError> {
        let artifact = Self {
            path: path.to_owned(),
            kind,
            sha256: sha256.map(str::to_owned),
        };
        artifact.validate_at("expected_artifact")?;
        Ok(artifact)
    }

    /// Returns the expected artifact path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the expected artifact kind.
    #[must_use]
    pub fn kind(&self) -> ArtifactKind {
        self.kind
    }

    /// Returns the optional expected SHA-256 digest.
    #[must_use]
    pub fn sha256(&self) -> Option<&str> {
        self.sha256.as_deref()
    }

    fn validate_at(&self, field: &str) -> Result<(), EvalError> {
        validate_relative_path(&format!("{field}.path"), &self.path)?;
        if let Some(digest) = self.sha256.as_deref() {
            validate_sha256(&format!("{field}.sha256"), digest)?;
        }
        Ok(())
    }
}

/// Provider-neutral kinds for expected task artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// A regular file.
    File,
    /// A directory tree.
    Directory,
    /// UTF-8 text.
    Text,
    /// Structured JSON.
    Json,
    /// A patch or diff.
    Diff,
}

/// A machine-checkable condition for task success.
#[derive(Debug, Clone, PartialEq, Eq, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SuccessCriterion {
    /// Requires a relative file path to exist.
    FileExists {
        /// Relative path to check.
        #[schemars(schema_with = "relative_path_schema")]
        path: String,
    },
    /// Requires a file to contain an exact text fragment.
    FileContains {
        /// Relative path to inspect.
        #[schemars(schema_with = "relative_path_schema")]
        path: String,
        /// Text fragment to find.
        #[schemars(
            length(min = 1, max = MAX_DIFF_CHARS),
            regex(pattern = NON_BLANK_TEXT_PATTERN)
        )]
        text: String,
    },
    /// Runs an explicit command and requires a successful exit.
    CommandPasses {
        /// Executable token.
        #[schemars(
            length(min = 1, max = MAX_PATH_CHARS),
            regex(pattern = NON_BLANK_TEXT_PATTERN)
        )]
        program: String,
        /// Explicit command arguments.
        #[serde(default)]
        #[schemars(
        default,
        length(max = MAX_COMMAND_ARGS),
        inner(
            length(max = MAX_COMMAND_ARG_CHARS),
            regex(pattern = NON_BLANK_TEXT_PATTERN)
        )
    )]
        args: Vec<String>,
        /// Optional relative working directory.
        #[serde(default)]
        #[schemars(default, schema_with = "optional_relative_path_schema")]
        working_dir: Option<String>,
        /// Optional command timeout.
        #[serde(default, deserialize_with = "deserialize_option_u64")]
        #[schemars(default, schema_with = "optional_positive_u64_schema")]
        timeout_seconds: Option<u64>,
    },
    /// Compares a relative file's content with an expected diff.
    DiffMatches {
        /// Relative path to inspect.
        #[schemars(schema_with = "relative_path_schema")]
        path: String,
        /// Expected diff content.
        #[schemars(
            length(min = 1, max = MAX_DIFF_CHARS),
            regex(pattern = NON_BLANK_TEXT_PATTERN)
        )]
        expected: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SuccessCriterionWire {
    FileExists {
        #[schemars(schema_with = "relative_path_schema")]
        path: String,
    },
    FileContains {
        #[schemars(schema_with = "relative_path_schema")]
        path: String,
        #[schemars(
            length(min = 1, max = MAX_DIFF_CHARS),
            regex(pattern = NON_BLANK_TEXT_PATTERN)
        )]
        text: String,
    },
    CommandPasses {
        #[schemars(
            length(min = 1, max = MAX_PATH_CHARS),
            regex(pattern = NON_BLANK_TEXT_PATTERN)
        )]
        program: String,
        #[serde(default)]
        #[schemars(
        default,
        length(max = MAX_COMMAND_ARGS),
        inner(
            length(max = MAX_COMMAND_ARG_CHARS),
            regex(pattern = NON_BLANK_TEXT_PATTERN)
        )
    )]
        args: Vec<String>,
        #[serde(default)]
        #[schemars(default, schema_with = "optional_relative_path_schema")]
        working_dir: Option<String>,
        #[serde(default, deserialize_with = "deserialize_option_u64")]
        #[schemars(default, schema_with = "optional_positive_u64_schema")]
        timeout_seconds: Option<u64>,
    },
    DiffMatches {
        #[schemars(schema_with = "relative_path_schema")]
        path: String,
        #[schemars(
            length(min = 1, max = MAX_DIFF_CHARS),
            regex(pattern = NON_BLANK_TEXT_PATTERN)
        )]
        expected: String,
    },
}

impl TryFrom<SuccessCriterionWire> for SuccessCriterion {
    type Error = EvalError;

    fn try_from(wire: SuccessCriterionWire) -> Result<Self, Self::Error> {
        let criterion = match wire {
            SuccessCriterionWire::FileExists { path } => Self::FileExists { path },
            SuccessCriterionWire::FileContains { path, text } => Self::FileContains { path, text },
            SuccessCriterionWire::CommandPasses {
                program,
                args,
                working_dir,
                timeout_seconds,
            } => Self::CommandPasses {
                program,
                args,
                working_dir,
                timeout_seconds,
            },
            SuccessCriterionWire::DiffMatches { path, expected } => {
                Self::DiffMatches { path, expected }
            }
        };
        criterion.validate()?;
        Ok(criterion)
    }
}

impl<'de> Deserialize<'de> for SuccessCriterion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SuccessCriterionWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(de::Error::custom)
    }
}

impl Serialize for SuccessCriterion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate().map_err(S::Error::custom)?;
        let wire = match self {
            Self::FileExists { path } => SuccessCriterionWire::FileExists { path: path.clone() },
            Self::FileContains { path, text } => SuccessCriterionWire::FileContains {
                path: path.clone(),
                text: text.clone(),
            },
            Self::CommandPasses {
                program,
                args,
                working_dir,
                timeout_seconds,
            } => SuccessCriterionWire::CommandPasses {
                program: program.clone(),
                args: args.clone(),
                working_dir: working_dir.clone(),
                timeout_seconds: *timeout_seconds,
            },
            Self::DiffMatches { path, expected } => SuccessCriterionWire::DiffMatches {
                path: path.clone(),
                expected: expected.clone(),
            },
        };
        wire.serialize(serializer)
    }
}

impl SuccessCriterion {
    /// Validates paths, command arguments, and expected text in this criterion.
    pub fn validate(&self) -> Result<(), EvalError> {
        self.validate_at("success_criteria[0]")
    }

    fn validate_indexed(&self, index: usize) -> Result<(), EvalError> {
        self.validate_at(&format!("success_criteria[{index}]"))
    }

    fn validate_at(&self, field: &str) -> Result<(), EvalError> {
        match self {
            Self::FileExists { path } => validate_relative_path(&format!("{field}.path"), path),
            Self::FileContains { path, text } => {
                validate_relative_path(&format!("{field}.path"), path)?;
                validate_text_field(&format!("{field}.text"), text, MAX_DIFF_CHARS)
            }
            Self::CommandPasses {
                program,
                args,
                working_dir,
                timeout_seconds,
            } => CommandSpec {
                program: program.clone(),
                args: args.clone(),
                working_dir: working_dir.clone(),
                timeout_seconds: *timeout_seconds,
            }
            .validate(field),
            Self::DiffMatches { path, expected } => {
                validate_relative_path(&format!("{field}.path"), path)?;
                validate_text_field(&format!("{field}.expected"), expected, MAX_DIFF_CHARS)
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskSpecWire {
    #[serde(deserialize_with = "deserialize_u32")]
    #[schemars(schema_with = "version_schema")]
    schema_version: u32,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = IDENTIFIER_PATTERN)
    )]
    task_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = IDENTIFIER_PATTERN)
    )]
    task_version: String,
    #[schemars(
        length(min = 1, max = MAX_DESCRIPTION_CHARS),
        regex(pattern = NON_BLANK_TEXT_PATTERN)
    )]
    description: String,
    repository: RepositorySpec,
    #[schemars(
        length(min = 1),
        inner(length(max = MAX_SCOPE_CHARS), regex(pattern = RELATIVE_PATH_PATTERN))
    )]
    write_scope: Vec<String>,
    #[serde(default)]
    setup: Vec<CommandSpec>,
    #[serde(default)]
    tests: Vec<CommandSpec>,
    #[serde(deserialize_with = "deserialize_u64")]
    #[schemars(schema_with = "task_timeout_schema")]
    timeout_seconds: u64,
    #[serde(default)]
    #[schemars(default)]
    resource_limits: ResourceLimits,
    #[serde(default)]
    #[schemars(default)]
    risk_policy: RiskPolicy,
    #[schemars(length(min = 1))]
    success_criteria: Vec<SuccessCriterion>,
    #[serde(default)]
    #[schemars(default)]
    expected_artifacts: Vec<ExpectedArtifactWire>,
    #[schemars(
        default,
        length(min = 1, max = MAX_DIFF_CHARS),
        regex(pattern = NON_BLANK_TEXT_PATTERN)
    )]
    expected_diff: Option<String>,
}

/// A validated, versioned manifest describing one evaluation task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskSpec {
    #[schemars(schema_with = "version_schema")]
    schema_version: u32,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = IDENTIFIER_PATTERN)
    )]
    task_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = IDENTIFIER_PATTERN)
    )]
    task_version: String,
    #[schemars(
        length(min = 1, max = MAX_DESCRIPTION_CHARS),
        regex(pattern = NON_BLANK_TEXT_PATTERN)
    )]
    description: String,
    repository: RepositorySpec,
    #[schemars(
        length(min = 1),
        inner(length(max = MAX_SCOPE_CHARS), regex(pattern = RELATIVE_PATH_PATTERN))
    )]
    write_scope: Vec<String>,
    #[serde(default)]
    #[schemars(default)]
    setup: Vec<CommandSpec>,
    #[serde(default)]
    #[schemars(default)]
    tests: Vec<CommandSpec>,
    #[schemars(schema_with = "task_timeout_schema")]
    timeout_seconds: u64,
    #[serde(default)]
    #[schemars(default)]
    resource_limits: ResourceLimits,
    #[serde(default)]
    #[schemars(default)]
    risk_policy: RiskPolicy,
    #[schemars(length(min = 1))]
    success_criteria: Vec<SuccessCriterion>,
    #[serde(default)]
    #[schemars(default)]
    expected_artifacts: Vec<ExpectedArtifact>,
    #[schemars(
        default,
        length(min = 1, max = MAX_DIFF_CHARS),
        regex(pattern = NON_BLANK_TEXT_PATTERN)
    )]
    expected_diff: Option<String>,
}

impl TaskSpec {
    /// Parses and validates a strict TOML task manifest.
    pub fn from_toml(source: &str) -> Result<Self, EvalError> {
        let wire: TaskSpecWire = toml::from_str(source)?;
        Self::from_wire(wire)
    }

    /// Serializes this validated task manifest as canonical TOML.
    pub fn to_toml(&self) -> Result<String, EvalError> {
        Ok(toml::to_string_pretty(self)?)
    }

    /// Returns the manifest schema version.
    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the stable task identifier.
    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// Returns the task version.
    #[must_use]
    pub fn task_version(&self) -> &str {
        &self.task_version
    }

    /// Returns the task description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the repository or image specification.
    #[must_use]
    pub fn repository(&self) -> &RepositorySpec {
        &self.repository
    }

    /// Returns the declared writable path patterns.
    #[must_use]
    pub fn write_scope(&self) -> &[String] {
        &self.write_scope
    }

    /// Returns setup commands in declaration order.
    #[must_use]
    pub fn setup(&self) -> &[CommandSpec] {
        &self.setup
    }

    /// Returns verification commands in declaration order.
    #[must_use]
    pub fn tests(&self) -> &[CommandSpec] {
        &self.tests
    }

    /// Returns the task timeout in seconds.
    #[must_use]
    pub fn timeout_seconds(&self) -> u64 {
        self.timeout_seconds
    }

    /// Returns resource ceilings.
    #[must_use]
    pub fn resource_limits(&self) -> &ResourceLimits {
        &self.resource_limits
    }

    /// Returns the task's risk policy.
    #[must_use]
    pub fn risk_policy(&self) -> RiskPolicy {
        self.risk_policy
    }

    /// Returns machine-checkable success conditions.
    #[must_use]
    pub fn success_criteria(&self) -> &[SuccessCriterion] {
        &self.success_criteria
    }

    /// Returns expected artifacts.
    #[must_use]
    pub fn expected_artifacts(&self) -> &[ExpectedArtifact] {
        &self.expected_artifacts
    }

    /// Returns an optional expected diff.
    #[must_use]
    pub fn expected_diff(&self) -> Option<&str> {
        self.expected_diff.as_deref()
    }

    fn from_wire(wire: TaskSpecWire) -> Result<Self, EvalError> {
        if wire.schema_version != TASK_SCHEMA_VERSION {
            return Err(EvalError::UnsupportedVersion {
                kind: "task manifest",
                found: wire.schema_version,
                supported: TASK_SCHEMA_VERSION,
            });
        }
        validate_identifier("task_id", &wire.task_id, 128)?;
        validate_identifier("task_version", &wire.task_version, 128)?;
        validate_text("description", &wire.description, MAX_DESCRIPTION_CHARS)?;
        validate_repository(&wire.repository)?;
        validate_scope(&wire.write_scope)?;
        for (index, command) in wire.setup.iter().enumerate() {
            command.validate(&format!("setup[{index}]"))?;
        }
        for (index, command) in wire.tests.iter().enumerate() {
            command.validate(&format!("tests[{index}]"))?;
        }
        if wire.timeout_seconds == 0 {
            return Err(invalid_field(
                "timeout_seconds",
                "must be greater than zero",
            ));
        }
        if wire.timeout_seconds > 7 * 24 * 60 * 60 {
            return Err(invalid_field(
                "timeout_seconds",
                "must not exceed seven days",
            ));
        }
        wire.resource_limits.validate()?;
        if wire.success_criteria.is_empty() {
            return Err(invalid_field(
                "success_criteria",
                "must contain at least one criterion",
            ));
        }
        for (index, criterion) in wire.success_criteria.iter().enumerate() {
            criterion.validate_indexed(index)?;
        }
        let expected_artifacts = wire
            .expected_artifacts
            .into_iter()
            .enumerate()
            .map(|(index, artifact)| {
                ExpectedArtifact::from_wire_at(artifact, &format!("expected_artifacts[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(expected_diff) = wire.expected_diff.as_deref() {
            validate_text("expected_diff", expected_diff, MAX_DIFF_CHARS)?;
        }

        Ok(Self {
            schema_version: wire.schema_version,
            task_id: wire.task_id,
            task_version: wire.task_version,
            description: wire.description,
            repository: wire.repository,
            write_scope: wire.write_scope,
            setup: wire.setup,
            tests: wire.tests,
            timeout_seconds: wire.timeout_seconds,
            resource_limits: wire.resource_limits,
            risk_policy: wire.risk_policy,
            success_criteria: wire.success_criteria,
            expected_artifacts,
            expected_diff: wire.expected_diff,
        })
    }
}

impl<'de> Deserialize<'de> for TaskSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TaskSpecWire::deserialize(deserializer)?;
        Self::from_wire(wire).map_err(de::Error::custom)
    }
}
