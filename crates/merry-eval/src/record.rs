use crate::error::{
    EvalError, IDENTIFIER_PATTERN, NON_BLANK_TEXT_PATTERN, invalid_field, optional_sha256_schema,
    relative_path_schema, validate_identifier, validate_relative_path, validate_text,
};
use crate::manifest::ArtifactKind;
use crate::numeric::{
    deserialize_option_u64, deserialize_u32, deserialize_u64, nonnegative_u64_schema,
    optional_nonnegative_u64_schema, positive_u32_schema, version_schema,
};
use schemars::JsonSchema;

#[path = "record_schema.rs"]
mod record_schema;
use record_schema::evaluation_record_schema;
use serde::{Deserialize, Serialize, Serializer, ser::Error as SerdeError};

/// The only evaluation-run schema version understood by this crate.
pub const EVALUATION_RUN_SCHEMA_VERSION: u32 = 1;
/// The only JSONL evaluation-record schema version understood by this crate.
pub const EVALUATION_RECORD_SCHEMA_VERSION: u32 = 1;

const MAX_JSONL_BYTES: usize = 4 * 1024 * 1024;
const MAX_RECORD_ITEMS: usize = 1024;

/// Terminal state recorded for an evaluation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationStatus {
    /// The task satisfied its success criteria.
    Passed,
    /// The task ran but did not satisfy its success criteria.
    Failed,
    /// A policy or permission decision prevented completion.
    Blocked,
    /// The run was cancelled before completion.
    Cancelled,
    /// The harness could not complete the run due to an infrastructure error.
    Error,
}

/// A normalized reason for a non-passing evaluation result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// The model response was invalid or incomplete.
    Model,
    /// A tool invocation failed.
    Tool,
    /// A permission or risk policy denied an action.
    Permission,
    /// The runtime failed independently of a tool or model.
    Runtime,
    /// A success criterion or verification command failed.
    Test,
    /// A configured timeout expired.
    Timeout,
    /// The run was cancelled by its owner.
    Cancelled,
}

/// Metadata identifying one evaluation attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, try_from = "EvaluationRunWire")]
pub struct EvaluationRun {
    #[schemars(schema_with = "version_schema")]
    schema_version: u32,
    #[schemars(length(min = 1, max = 128), regex(pattern = IDENTIFIER_PATTERN))]
    run_id: String,
    #[schemars(length(min = 1, max = 128), regex(pattern = IDENTIFIER_PATTERN))]
    task_id: String,
    #[schemars(length(min = 1, max = 128), regex(pattern = IDENTIFIER_PATTERN))]
    task_version: String,
    #[schemars(length(min = 1, max = 128), regex(pattern = IDENTIFIER_PATTERN))]
    runner_version: String,
    #[serde(default)]
    #[schemars(
        default,
        length(min = 1, max = 256),
        regex(pattern = IDENTIFIER_PATTERN)
    )]
    source_commit: Option<String>,
    #[serde(default)]
    #[schemars(
        default,
        length(min = 1, max = 128),
        regex(pattern = IDENTIFIER_PATTERN)
    )]
    provider: Option<String>,
    #[serde(default)]
    #[schemars(
        default,
        length(min = 1, max = 256),
        regex(pattern = IDENTIFIER_PATTERN)
    )]
    model: Option<String>,
    #[serde(default)]
    #[schemars(default, schema_with = "optional_sha256_schema")]
    prompt_hash: Option<String>,
    #[serde(default)]
    #[schemars(default, schema_with = "optional_sha256_schema")]
    profile_hash: Option<String>,
    #[serde(default)]
    #[schemars(default, schema_with = "optional_sha256_schema")]
    tool_schema_hash: Option<String>,
    #[schemars(schema_with = "nonnegative_u64_schema")]
    started_at_ms: u64,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EvaluationRunWire {
    #[serde(deserialize_with = "deserialize_u32")]
    #[schemars(schema_with = "version_schema")]
    schema_version: u32,
    #[schemars(length(min = 1, max = 128), regex(pattern = IDENTIFIER_PATTERN))]
    run_id: String,
    #[schemars(length(min = 1, max = 128), regex(pattern = IDENTIFIER_PATTERN))]
    task_id: String,
    #[schemars(length(min = 1, max = 128), regex(pattern = IDENTIFIER_PATTERN))]
    task_version: String,
    #[schemars(length(min = 1, max = 128), regex(pattern = IDENTIFIER_PATTERN))]
    runner_version: String,
    #[serde(default)]
    #[schemars(
        default,
        length(min = 1, max = 256),
        regex(pattern = IDENTIFIER_PATTERN)
    )]
    source_commit: Option<String>,
    #[serde(default)]
    #[schemars(
        default,
        length(min = 1, max = 128),
        regex(pattern = IDENTIFIER_PATTERN)
    )]
    provider: Option<String>,
    #[serde(default)]
    #[schemars(
        default,
        length(min = 1, max = 256),
        regex(pattern = IDENTIFIER_PATTERN)
    )]
    model: Option<String>,
    #[serde(default)]
    #[schemars(default, schema_with = "optional_sha256_schema")]
    prompt_hash: Option<String>,
    #[serde(default)]
    #[schemars(default, schema_with = "optional_sha256_schema")]
    profile_hash: Option<String>,
    #[serde(default)]
    #[schemars(default, schema_with = "optional_sha256_schema")]
    tool_schema_hash: Option<String>,
    #[serde(deserialize_with = "deserialize_u64")]
    #[schemars(schema_with = "nonnegative_u64_schema")]
    started_at_ms: u64,
}

impl TryFrom<EvaluationRunWire> for EvaluationRun {
    type Error = EvalError;

    fn try_from(wire: EvaluationRunWire) -> Result<Self, Self::Error> {
        let run = Self {
            schema_version: wire.schema_version,
            run_id: wire.run_id,
            task_id: wire.task_id,
            task_version: wire.task_version,
            runner_version: wire.runner_version,
            source_commit: wire.source_commit,
            provider: wire.provider,
            model: wire.model,
            prompt_hash: wire.prompt_hash,
            profile_hash: wire.profile_hash,
            tool_schema_hash: wire.tool_schema_hash,
            started_at_ms: wire.started_at_ms,
        };
        run.validate()?;
        Ok(run)
    }
}

impl EvaluationRun {
    /// Creates a run with stable task identity and a start timestamp.
    pub fn new(
        run_id: &str,
        task_id: &str,
        task_version: &str,
        started_at_ms: u64,
    ) -> Result<Self, EvalError> {
        validate_identifier("run_id", run_id, 128)?;
        validate_identifier("task_id", task_id, 128)?;
        validate_identifier("task_version", task_version, 128)?;
        Ok(Self {
            schema_version: EVALUATION_RUN_SCHEMA_VERSION,
            run_id: run_id.to_owned(),
            task_id: task_id.to_owned(),
            task_version: task_version.to_owned(),
            runner_version: env!("CARGO_PKG_VERSION").to_owned(),
            source_commit: None,
            provider: None,
            model: None,
            prompt_hash: None,
            profile_hash: None,
            tool_schema_hash: None,
            started_at_ms,
        })
    }

    /// Adds the source revision used to prepare the evaluation workspace.
    pub fn with_source_commit(mut self, source_commit: &str) -> Result<Self, EvalError> {
        validate_identifier("source_commit", source_commit, 256)?;
        self.source_commit = Some(source_commit.to_owned());
        Ok(self)
    }

    /// Adds a provider label without storing provider wire payloads.
    pub fn with_provider(mut self, provider: &str) -> Result<Self, EvalError> {
        validate_identifier("provider", provider, 128)?;
        self.provider = Some(provider.to_owned());
        Ok(self)
    }

    /// Adds a model label without storing a request or response.
    pub fn with_model(mut self, model: &str) -> Result<Self, EvalError> {
        validate_identifier("model", model, 256)?;
        self.model = Some(model.to_owned());
        Ok(self)
    }

    /// Sets the version of the evaluation runner that produced the record.
    pub fn with_runner_version(mut self, runner_version: &str) -> Result<Self, EvalError> {
        validate_identifier("runner_version", runner_version, 128)?;
        self.runner_version = runner_version.to_owned();
        Ok(self)
    }

    /// Adds the stable hash of provider-visible prompt text used for the run.
    pub fn with_prompt_hash(mut self, prompt_hash: &str) -> Result<Self, EvalError> {
        validate_hash("prompt_hash", prompt_hash)?;
        self.prompt_hash = Some(prompt_hash.to_owned());
        Ok(self)
    }

    /// Adds the stable coding-profile hash used for the run.
    pub fn with_profile_hash(mut self, profile_hash: &str) -> Result<Self, EvalError> {
        validate_hash("profile_hash", profile_hash)?;
        self.profile_hash = Some(profile_hash.to_owned());
        Ok(self)
    }

    /// Adds the stable tool-schema hash used for the run.
    pub fn with_tool_schema_hash(mut self, tool_schema_hash: &str) -> Result<Self, EvalError> {
        validate_hash("tool_schema_hash", tool_schema_hash)?;
        self.tool_schema_hash = Some(tool_schema_hash.to_owned());
        Ok(self)
    }

    /// Returns the run schema version.
    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the stable run identifier.
    #[must_use]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Returns the task identifier.
    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// Returns the task version.
    #[must_use]
    pub fn task_version(&self) -> &str {
        &self.task_version
    }

    /// Returns the evaluation runner version.
    #[must_use]
    pub fn runner_version(&self) -> &str {
        &self.runner_version
    }

    /// Returns the source revision, when recorded.
    #[must_use]
    pub fn source_commit(&self) -> Option<&str> {
        self.source_commit.as_deref()
    }

    /// Returns the provider label, when recorded.
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// Returns the model label, when recorded.
    #[must_use]
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Returns the prompt hash, when recorded.
    #[must_use]
    pub fn prompt_hash(&self) -> Option<&str> {
        self.prompt_hash.as_deref()
    }

    /// Returns the coding-profile hash, when recorded.
    #[must_use]
    pub fn profile_hash(&self) -> Option<&str> {
        self.profile_hash.as_deref()
    }

    /// Returns the tool-schema hash, when recorded.
    #[must_use]
    pub fn tool_schema_hash(&self) -> Option<&str> {
        self.tool_schema_hash.as_deref()
    }

    /// Returns the start timestamp in milliseconds since the Unix epoch.
    #[must_use]
    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    fn validate(&self) -> Result<(), EvalError> {
        if self.schema_version != EVALUATION_RUN_SCHEMA_VERSION {
            return Err(EvalError::UnsupportedVersion {
                kind: "evaluation run",
                found: self.schema_version,
                supported: EVALUATION_RUN_SCHEMA_VERSION,
            });
        }
        validate_identifier("run_id", &self.run_id, 128)?;
        validate_identifier("task_id", &self.task_id, 128)?;
        validate_identifier("task_version", &self.task_version, 128)?;
        if let Some(source_commit) = self.source_commit.as_deref() {
            validate_identifier("source_commit", source_commit, 256)?;
        }
        if let Some(provider) = self.provider.as_deref() {
            validate_identifier("provider", provider, 128)?;
        }
        validate_identifier("runner_version", &self.runner_version, 128)?;
        if let Some(model) = self.model.as_deref() {
            validate_identifier("model", model, 256)?;
        }
        if let Some(prompt_hash) = self.prompt_hash.as_deref() {
            validate_hash("prompt_hash", prompt_hash)?;
        }
        if let Some(profile_hash) = self.profile_hash.as_deref() {
            validate_hash("profile_hash", profile_hash)?;
        }
        if let Some(tool_schema_hash) = self.tool_schema_hash.as_deref() {
            validate_hash("tool_schema_hash", tool_schema_hash)?;
        }
        Ok(())
    }
}

/// Result of one verification command, without its potentially sensitive output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, try_from = "TestResultWire")]
pub struct TestResult {
    #[schemars(
        length(min = 1, max = 256),
        regex(pattern = NON_BLANK_TEXT_PATTERN)
    )]
    name: String,
    passed: bool,
    #[serde(default, deserialize_with = "deserialize_option_u64")]
    #[schemars(default, schema_with = "optional_nonnegative_u64_schema")]
    duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TestResultWire {
    #[schemars(
        length(min = 1, max = 256),
        regex(pattern = NON_BLANK_TEXT_PATTERN)
    )]
    name: String,
    passed: bool,
    #[serde(default, deserialize_with = "deserialize_option_u64")]
    #[schemars(default, schema_with = "optional_nonnegative_u64_schema")]
    duration_ms: Option<u64>,
}

impl TryFrom<TestResultWire> for TestResult {
    type Error = EvalError;

    fn try_from(wire: TestResultWire) -> Result<Self, Self::Error> {
        Self::new(&wire.name, wire.passed, wire.duration_ms)
    }
}

impl TestResult {
    /// Creates a normalized test result.
    pub fn new(name: &str, passed: bool, duration_ms: Option<u64>) -> Result<Self, EvalError> {
        validate_text("test.name", name, 256)?;
        Ok(Self {
            name: name.to_owned(),
            passed,
            duration_ms,
        })
    }

    /// Returns the test name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns whether the test passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.passed
    }

    /// Returns the optional test duration.
    #[must_use]
    pub fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }
}

/// Count of actions denied by a named policy boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, try_from = "DenialRecordWire")]
pub struct DenialRecord {
    #[schemars(length(min = 1, max = 128), regex(pattern = IDENTIFIER_PATTERN))]
    code: String,
    #[schemars(schema_with = "positive_u32_schema")]
    count: u32,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DenialRecordWire {
    #[schemars(length(min = 1, max = 128), regex(pattern = IDENTIFIER_PATTERN))]
    code: String,
    #[serde(deserialize_with = "deserialize_u32")]
    #[schemars(schema_with = "positive_u32_schema")]
    count: u32,
}

impl TryFrom<DenialRecordWire> for DenialRecord {
    type Error = EvalError;

    fn try_from(wire: DenialRecordWire) -> Result<Self, Self::Error> {
        Self::new(&wire.code, wire.count)
    }
}

impl DenialRecord {
    /// Creates a non-empty denial count.
    pub fn new(code: &str, count: u32) -> Result<Self, EvalError> {
        validate_identifier("denial.code", code, 128)?;
        if count == 0 {
            return Err(invalid_field("denial.count", "must be greater than zero"));
        }
        Ok(Self {
            code: code.to_owned(),
            count,
        })
    }

    /// Returns the stable denial code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the number of occurrences.
    #[must_use]
    pub fn count(&self) -> u32 {
        self.count
    }
}

/// A reference to an artifact produced by an evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, try_from = "ArtifactRecordWire")]
pub struct ArtifactRecord {
    #[schemars(schema_with = "relative_path_schema")]
    path: String,
    kind: ArtifactKind,
    #[schemars(schema_with = "optional_sha256_schema")]
    sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ArtifactRecordWire {
    #[schemars(schema_with = "relative_path_schema")]
    path: String,
    kind: ArtifactKind,
    #[serde(default)]
    #[schemars(default, schema_with = "optional_sha256_schema")]
    sha256: Option<String>,
}

impl TryFrom<ArtifactRecordWire> for ArtifactRecord {
    type Error = EvalError;

    fn try_from(wire: ArtifactRecordWire) -> Result<Self, Self::Error> {
        Self::new(&wire.path, wire.kind, wire.sha256.as_deref())
    }
}

impl ArtifactRecord {
    /// Creates a relative artifact reference without copying its contents.
    pub fn new(path: &str, kind: ArtifactKind, sha256: Option<&str>) -> Result<Self, EvalError> {
        validate_relative_path("artifact.path", path)?;
        if let Some(sha256) = sha256 {
            validate_hash("artifact.sha256", sha256)?;
        }
        Ok(Self {
            path: path.to_owned(),
            kind,
            sha256: sha256.map(str::to_owned),
        })
    }

    /// Returns the relative artifact path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Returns the provider-neutral artifact kind.
    #[must_use]
    pub fn kind(&self) -> ArtifactKind {
        self.kind
    }

    /// Returns the optional artifact digest.
    #[must_use]
    pub fn sha256(&self) -> Option<&str> {
        self.sha256.as_deref()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EvaluationRecordWire {
    #[serde(deserialize_with = "deserialize_u32")]
    #[schemars(schema_with = "version_schema")]
    schema_version: u32,
    run: EvaluationRun,
    status: EvaluationStatus,
    #[serde(default)]
    #[schemars(default)]
    failure_kind: Option<FailureKind>,
    #[serde(deserialize_with = "deserialize_u32")]
    turns: u32,
    #[serde(deserialize_with = "deserialize_u32")]
    tool_calls: u32,
    #[serde(deserialize_with = "deserialize_u32")]
    retries: u32,
    #[serde(default, deserialize_with = "deserialize_option_u64")]
    #[schemars(default)]
    latency_ms: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_option_u64")]
    #[schemars(default)]
    cost_micros: Option<u64>,
    #[schemars(length(max = MAX_RECORD_ITEMS))]
    tests: Vec<TestResult>,
    #[schemars(length(max = MAX_RECORD_ITEMS))]
    denials: Vec<DenialRecord>,
    #[schemars(length(max = MAX_RECORD_ITEMS))]
    artifacts: Vec<ArtifactRecord>,
}

/// One deterministic JSONL result emitted by an evaluation harness.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[schemars(schema_with = "evaluation_record_schema")]
#[serde(try_from = "EvaluationRecordWire")]
pub struct EvaluationRecord {
    #[schemars(schema_with = "version_schema")]
    schema_version: u32,
    run: EvaluationRun,
    status: EvaluationStatus,
    #[serde(default)]
    #[schemars(default)]
    failure_kind: Option<FailureKind>,
    #[serde(deserialize_with = "deserialize_u32")]
    turns: u32,
    #[serde(deserialize_with = "deserialize_u32")]
    tool_calls: u32,
    #[serde(deserialize_with = "deserialize_u32")]
    retries: u32,
    #[serde(default, deserialize_with = "deserialize_option_u64")]
    #[schemars(default)]
    latency_ms: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_option_u64")]
    #[schemars(default)]
    cost_micros: Option<u64>,
    #[schemars(length(max = MAX_RECORD_ITEMS))]
    tests: Vec<TestResult>,
    #[schemars(length(max = MAX_RECORD_ITEMS))]
    denials: Vec<DenialRecord>,
    #[schemars(length(max = MAX_RECORD_ITEMS))]
    artifacts: Vec<ArtifactRecord>,
}

impl TryFrom<EvaluationRecordWire> for EvaluationRecord {
    type Error = EvalError;

    fn try_from(wire: EvaluationRecordWire) -> Result<Self, Self::Error> {
        let record = Self {
            schema_version: wire.schema_version,
            run: wire.run,
            status: wire.status,
            failure_kind: wire.failure_kind,
            turns: wire.turns,
            tool_calls: wire.tool_calls,
            retries: wire.retries,
            latency_ms: wire.latency_ms,
            cost_micros: wire.cost_micros,
            tests: wire.tests,
            denials: wire.denials,
            artifacts: wire.artifacts,
        };
        record.validate_inner()?;
        Ok(record)
    }
}

impl Serialize for EvaluationRecord {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.validate_inner().map_err(S::Error::custom)?;
        let wire = EvaluationRecordWire {
            schema_version: self.schema_version,
            run: self.run.clone(),
            status: self.status,
            failure_kind: self.failure_kind,
            turns: self.turns,
            tool_calls: self.tool_calls,
            retries: self.retries,
            latency_ms: self.latency_ms,
            cost_micros: self.cost_micros,
            tests: self.tests.clone(),
            denials: self.denials.clone(),
            artifacts: self.artifacts.clone(),
        };
        wire.serialize(serializer)
    }
}

impl EvaluationRecord {
    /// Creates a provisional result that can be completed with the builder methods.
    #[must_use]
    pub fn new(run: EvaluationRun, status: EvaluationStatus) -> Self {
        Self {
            schema_version: EVALUATION_RECORD_SCHEMA_VERSION,
            run,
            status,
            failure_kind: None,
            turns: 0,
            tool_calls: 0,
            retries: 0,
            latency_ms: None,
            cost_micros: None,
            tests: Vec::new(),
            denials: Vec::new(),
            artifacts: Vec::new(),
        }
    }

    /// Creates and validates a complete result in one operation.
    pub fn try_new(
        run: EvaluationRun,
        status: EvaluationStatus,
        failure_kind: Option<FailureKind>,
    ) -> Result<Self, EvalError> {
        let mut record = Self::new(run, status);
        record.failure_kind = failure_kind;
        record.validate_inner()?;
        Ok(record)
    }

    /// Validates and returns a completed result after builder methods are applied.
    pub fn finish(self) -> Result<Self, EvalError> {
        self.validate_inner()?;
        Ok(self)
    }

    /// Adds a normalized failure category to a non-passing result.
    pub fn with_failure_kind(mut self, failure_kind: FailureKind) -> Self {
        self.failure_kind = Some(failure_kind);
        self
    }

    /// Records aggregate execution counters and optional timing/cost metrics.
    pub fn with_metrics(
        mut self,
        turns: u32,
        tool_calls: u32,
        retries: u32,
        latency_ms: Option<u64>,
        cost_micros: Option<u64>,
    ) -> Self {
        self.turns = turns;
        self.tool_calls = tool_calls;
        self.retries = retries;
        self.latency_ms = latency_ms;
        self.cost_micros = cost_micros;
        self
    }

    /// Appends a verification result while preserving declaration order.
    pub fn with_test(mut self, test: TestResult) -> Self {
        self.tests.push(test);
        self
    }

    /// Appends a policy-denial summary while preserving event order.
    pub fn with_denial(mut self, denial: DenialRecord) -> Self {
        self.denials.push(denial);
        self
    }

    /// Appends an artifact reference without copying artifact contents.
    pub fn with_artifact(mut self, artifact: ArtifactRecord) -> Self {
        self.artifacts.push(artifact);
        self
    }

    /// Returns the record schema version.
    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the run metadata.
    #[must_use]
    pub fn run(&self) -> &EvaluationRun {
        &self.run
    }

    /// Returns the terminal evaluation status.
    #[must_use]
    pub fn status(&self) -> EvaluationStatus {
        self.status
    }

    /// Returns the normalized failure category, when present.
    #[must_use]
    pub fn failure_kind(&self) -> Option<FailureKind> {
        self.failure_kind
    }

    /// Returns the number of model turns.
    #[must_use]
    pub fn turns(&self) -> u32 {
        self.turns
    }

    /// Returns the number of tool calls.
    #[must_use]
    pub fn tool_calls(&self) -> u32 {
        self.tool_calls
    }

    /// Returns the number of retries.
    #[must_use]
    pub fn retries(&self) -> u32 {
        self.retries
    }

    /// Returns the optional elapsed time in milliseconds.
    #[must_use]
    pub fn latency_ms(&self) -> Option<u64> {
        self.latency_ms
    }

    /// Returns the optional cost in provider-neutral micro-units.
    #[must_use]
    pub fn cost_micros(&self) -> Option<u64> {
        self.cost_micros
    }

    /// Returns verification results in declaration order.
    #[must_use]
    pub fn tests(&self) -> &[TestResult] {
        &self.tests
    }

    /// Returns policy-denial summaries in event order.
    #[must_use]
    pub fn denials(&self) -> &[DenialRecord] {
        &self.denials
    }

    /// Returns artifact references in declaration order.
    #[must_use]
    pub fn artifacts(&self) -> &[ArtifactRecord] {
        &self.artifacts
    }

    /// Validates the complete record, including nested values and status invariants.
    pub fn validate(&self) -> Result<(), EvalError> {
        self.validate_inner()
    }

    /// Serializes the record as exactly one newline-terminated JSONL line.
    pub fn to_jsonl(&self) -> Result<String, EvalError> {
        self.validate()?;
        // Escape Unicode line/paragraph separators so splitlines-style consumers see one physical line.
        let json = serde_json::to_string(self)?
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029");
        if json.len() + 1 > MAX_JSONL_BYTES {
            return Err(EvalError::InvalidRecordFraming(format!(
                "record exceeds the {} byte limit",
                MAX_JSONL_BYTES
            )));
        }
        Ok(format!("{json}\n"))
    }

    /// Parses and validates exactly one JSONL record line.
    pub fn from_jsonl(line: &str) -> Result<Self, EvalError> {
        if line.len() > MAX_JSONL_BYTES {
            return Err(EvalError::InvalidRecordFraming(format!(
                "record exceeds the {} byte limit",
                MAX_JSONL_BYTES
            )));
        }
        let payload = match line.strip_suffix('\n') {
            Some(payload) => payload.strip_suffix('\r').unwrap_or(payload),
            None => line,
        };
        if payload.is_empty() || payload.contains(['\r', '\n']) {
            return Err(EvalError::InvalidRecordFraming(
                "expected one JSON object with at most one line terminator".to_owned(),
            ));
        }
        if payload.trim_start() != payload || payload.trim_end() != payload {
            return Err(EvalError::InvalidRecordFraming(
                "leading and trailing whitespace are not allowed".to_owned(),
            ));
        }
        let record: Self = serde_json::from_str(payload)?;
        record.validate()?;
        Ok(record)
    }

    fn validate_inner(&self) -> Result<(), EvalError> {
        if self.schema_version != EVALUATION_RECORD_SCHEMA_VERSION {
            return Err(EvalError::UnsupportedVersion {
                kind: "evaluation record",
                found: self.schema_version,
                supported: EVALUATION_RECORD_SCHEMA_VERSION,
            });
        }
        self.run.validate()?;
        for (name, length) in [
            ("tests", self.tests.len()),
            ("denials", self.denials.len()),
            ("artifacts", self.artifacts.len()),
        ] {
            if length > MAX_RECORD_ITEMS {
                return Err(invalid_field(
                    name,
                    format!("must contain at most {MAX_RECORD_ITEMS} items"),
                ));
            }
        }
        for (index, test) in self.tests.iter().enumerate() {
            validate_text(&format!("tests[{index}].name"), &test.name, 256)?;
        }
        for (index, denial) in self.denials.iter().enumerate() {
            validate_identifier("denial.code", &denial.code, 128).map_err(|error| match error {
                EvalError::InvalidField { reason, .. } => {
                    invalid_field(format!("denials[{index}].code"), reason)
                }
                other => other,
            })?;
            if denial.count == 0 {
                return Err(invalid_field(
                    format!("denials[{index}].count"),
                    "must be greater than zero",
                ));
            }
        }
        for (index, artifact) in self.artifacts.iter().enumerate() {
            validate_relative_path("artifact.path", &artifact.path).map_err(
                |error| match error {
                    EvalError::InvalidField { reason, .. } => {
                        invalid_field(format!("artifacts[{index}].path"), reason)
                    }
                    other => other,
                },
            )?;
            if let Some(sha256) = artifact.sha256.as_deref() {
                validate_hash(&format!("artifacts[{index}].sha256"), sha256)?;
            }
        }
        match self.status {
            EvaluationStatus::Passed => {
                if self.failure_kind.is_some() {
                    return Err(invalid_field(
                        "failure_kind",
                        "must be absent for a passed evaluation",
                    ));
                }
                if self.tests.iter().any(|test| !test.passed) {
                    return Err(invalid_field(
                        "tests",
                        "must not contain a failed test for a passed evaluation",
                    ));
                }
            }
            EvaluationStatus::Failed
            | EvaluationStatus::Blocked
            | EvaluationStatus::Cancelled
            | EvaluationStatus::Error => {
                let Some(failure_kind) = self.failure_kind else {
                    return Err(invalid_field(
                        "failure_kind",
                        "is required for a non-passed evaluation",
                    ));
                };
                let matches_status = match self.status {
                    EvaluationStatus::Failed => true,
                    EvaluationStatus::Blocked => failure_kind == FailureKind::Permission,
                    EvaluationStatus::Cancelled => failure_kind == FailureKind::Cancelled,
                    EvaluationStatus::Error => {
                        matches!(failure_kind, FailureKind::Runtime | FailureKind::Timeout)
                    }
                    EvaluationStatus::Passed => false,
                };
                if !matches_status {
                    return Err(invalid_field(
                        "failure_kind",
                        "does not match the evaluation status",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn validate_hash(field: &str, value: &str) -> Result<(), EvalError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid_field(
            field,
            "must be a 64-character hexadecimal digest",
        ));
    }
    Ok(())
}
