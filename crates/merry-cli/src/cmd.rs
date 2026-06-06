use crate::cli_error::{CliError, debug_openai_usage_error, stdout_error, unexpected};
use crate::coding_runtime::{
    RuntimeRoleProviderConfig, coding_loop_workspace_roots, with_workspace_coding_loop_profile,
    workspace_tools_config,
};
use crate::config::MerryConfig;
use crate::provider_config::{
    OpenAiRuntimeConfig, openai_role_provider_config, openai_runtime_config,
};
use crate::runtime_config::automatic_compaction_config;
use merry_core::{ErrorInfo, PendingToolCall, ToolInputSchema, ToolName, ToolSpec};
use merry_llm::{ModelName, ModelProvider, ModelRetryPolicy};
use merry_provider_openai::OpenAiProvider;
use merry_runtime::{
    AgentLoopConfig, AgentLoopStatus, AutomaticCompactionConfig, RegisteredTool, Runtime,
    RuntimeModelRole, StepContext, StepInput, ToolExecutionContext, ToolExecutionOutcome,
    ToolExecutor, ToolExecutorFuture,
};
use merry_tool_workspace::WorkspaceCodingLoopProfile;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufWriter,
};
use tokio::process::Command as TokioCommand;

pub(crate) const CHECK_COMMAND_TOOL_NAME: &str = "cmd_check_command";
const CHECK_COMMAND_INVALID_ARGUMENTS_CODE: &str = "cmd_check_command_invalid_arguments";

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    #[arg(long, help = "Print only the structured CommandPlan JSON")]
    pub(crate) json: bool,

    #[arg(
        long = "no-prompt",
        help = "Do not ask to execute the generated shell command"
    )]
    pub(crate) no_prompt: bool,

    #[arg(required = true, allow_hyphen_values = true, value_name = "REQUEST")]
    pub(crate) request: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandPlan {
    #[schemars(description = "The exact shell command string to show to the user for execution.")]
    pub(crate) shell_command: String,
    #[schemars(description = "Short user-facing notes explaining what the command does.")]
    pub(crate) notes: Vec<String>,
    #[schemars(
        description = "Execution risks, destructive effects, or assumptions to review first."
    )]
    pub(crate) cautions: Vec<String>,
}

pub(crate) async fn run(args: &Args, merry_config: Option<&MerryConfig>) -> Result<(), CliError> {
    let config = openai_runtime_config(None, merry_config, debug_openai_usage_error)?;
    let OpenAiRuntimeConfig {
        primary,
        context_compaction,
        retry_policy,
        ..
    } = config;
    let root = env::current_dir().map_err(unexpected)?;
    let environment = CommandGenerationEnvironment::detect(&root);
    let runtime = build_runtime(RuntimeInput {
        session_id: "cmd",
        root: &root,
        environment: environment.clone(),
        provider: Arc::new(OpenAiProvider::new(primary.provider)),
        model: ModelName::new(&primary.model).map_err(unexpected)?,
        allow_hidden_workspace_paths: false,
        automatic_compaction: automatic_compaction_config(merry_config).map_err(unexpected)?,
        retry_policy,
        context_compaction: context_compaction
            .map(|config| {
                openai_role_provider_config(RuntimeModelRole::ContextCompaction, config, unexpected)
            })
            .transpose()?,
        skill_roots: merry_config
            .map(MerryConfig::skill_roots)
            .transpose()
            .map_err(unexpected)?
            .unwrap_or_default(),
    })?;
    let plan = generate_command_plan(&runtime, &args.request, &environment).await?;
    if args.json {
        write_command_plan_json(&plan, tokio::io::stdout()).await?;
    } else {
        write_command_plan_summary(&plan, tokio::io::stdout()).await?;
    }
    if args.json || args.no_prompt {
        return Ok(());
    }

    if prompt_execute_command_plan(
        &plan,
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await?
    {
        execute_shell_command_to_writer(&plan.shell_command, tokio::io::stdout()).await
    } else {
        Ok(())
    }
}

pub(crate) fn command_plan_final_output_contract()
-> Result<merry_runtime::FinalOutputContract, CliError> {
    let schema = ToolInputSchema::new(schemars::schema_for!(CommandPlan)).map_err(unexpected)?;
    merry_runtime::FinalOutputContract::new(schema).map_err(unexpected)
}

fn command_generation_loop_config() -> Result<AgentLoopConfig, CliError> {
    Ok(AgentLoopConfig::new(128)
        .map_err(unexpected)?
        .with_final_output_contract(command_plan_final_output_contract()?))
}

pub(crate) fn command_generation_prompt(
    request: &str,
    environment: &CommandGenerationEnvironment,
) -> String {
    format!(
        "\
You generate a shell command plan for the user's current workspace.

{environment}

Use only read-only workspace tools if you need to inspect files. Do not modify files. Do not run arbitrary commands.
Use `{check_tool}` to check whether optional programs exist before recommending them. This check only inspects PATH and shell builtins; it does not execute the program.
Return the final answer by calling the structured final output tool.

The shell_command field must contain exactly one shell command string for the human to review.
Prefer portable, explicit commands. Include notes for assumptions and cautions for risks.
Write notes and cautions in the user's current input language unless the user explicitly requests another language.
For file listing/search requests, interpret current directory, under this directory, and similar wording as recursive by default.
Use non-recursive limits such as find -maxdepth 1 only when the user explicitly asks for top-level files, direct children, or non-recursive search.
For multi-step read-only tasks, prefer a single shell pipeline or sh -c command that combines the steps when it is clearer than several separate commands.
Before using platform-specific or optional tools such as iostat, vmstat, ss, ip, ifconfig, nslookup, dig, lsof, jq, fd, rg, or git, call `{check_tool}` first unless the user explicitly asks for an unverified command string.
If a preferred optional command is unavailable, choose a reasonable fallback from available baseline commands and mention the tradeoff in notes.
Avoid interactive or unbounded commands. If a command samples repeatedly, include a finite count or timeout.

User request:
{request}",
        environment = environment.prompt_section(),
        check_tool = CHECK_COMMAND_TOOL_NAME,
    )
}

pub(crate) struct RuntimeInput<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) root: &'a Path,
    pub(crate) environment: CommandGenerationEnvironment,
    pub(crate) provider: Arc<dyn ModelProvider>,
    pub(crate) model: ModelName,
    pub(crate) allow_hidden_workspace_paths: bool,
    pub(crate) automatic_compaction: AutomaticCompactionConfig,
    pub(crate) retry_policy: Option<ModelRetryPolicy>,
    pub(crate) context_compaction: Option<RuntimeRoleProviderConfig>,
    pub(crate) skill_roots: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct CommandGenerationEnvironment {
    pub(crate) os: &'static str,
    pub(crate) arch: &'static str,
    pub(crate) family: &'static str,
    pub(crate) shell: String,
    pub(crate) cwd: PathBuf,
    pub(crate) path: Option<String>,
}

impl CommandGenerationEnvironment {
    pub(crate) fn detect(cwd: &Path) -> Self {
        Self {
            os: env::consts::OS,
            arch: env::consts::ARCH,
            family: env::consts::FAMILY,
            shell: env::var("SHELL").unwrap_or_else(|_| "sh".to_owned()),
            cwd: cwd.to_owned(),
            path: env::var("PATH").ok().filter(|path| !path.trim().is_empty()),
        }
    }

    fn prompt_section(&self) -> String {
        let path = self.path.as_deref().unwrap_or("<not set>");
        format!(
            "\
Runtime environment:
- os: {os}
- family: {family}
- arch: {arch}
- shell: {shell}
- cwd: {cwd}
- path: {path}
- baseline commands usually available on Unix-like systems: sh, test, printf, echo, pwd, cd, ls, find, grep, sed, awk, sort, uniq, wc, head, tail, cut, tr, xargs, cat, df, du, ps
",
            os = self.os,
            family = self.family,
            arch = self.arch,
            shell = self.shell,
            cwd = self.cwd.display(),
            path = path,
        )
    }

    fn command_availability(&self, program: &str) -> CommandAvailability {
        if is_shell_builtin(program) {
            return CommandAvailability {
                program: program.to_owned(),
                available: true,
                kind: "shell_builtin",
                path: None,
            };
        }

        let path = self
            .path
            .as_deref()
            .and_then(|paths| find_program_in_path(program, paths));
        CommandAvailability {
            program: program.to_owned(),
            available: path.is_some(),
            kind: "path",
            path,
        }
    }
}

pub(crate) fn build_runtime(input: RuntimeInput<'_>) -> Result<Runtime, CliError> {
    let session_id = merry_core::SessionId::new(input.session_id).map_err(unexpected)?;
    let mut builder = Runtime::builder(session_id)
        .automatic_compaction(input.automatic_compaction)
        .model_provider(input.provider, input.model);
    if let Some(policy) = input.retry_policy {
        builder = builder.model_retry_policy(policy);
    }
    if let Some(role_provider) = input.context_compaction {
        builder = builder.model_provider_for_role(
            role_provider.role,
            role_provider.provider,
            role_provider.model,
        );
    }
    let profile = WorkspaceCodingLoopProfile::new(workspace_tools_config(
        coding_loop_workspace_roots(input.root, &input.skill_roots),
        input.allow_hidden_workspace_paths,
        false,
        None,
    )?)
    .map_err(unexpected)?;
    let builder = with_workspace_coding_loop_profile(builder, profile)?
        .register_tool(cmd_check_command_tool(input.environment)?);
    builder.build().map_err(unexpected)
}

pub(crate) async fn generate_command_plan(
    runtime: &Runtime,
    request: &str,
    environment: &CommandGenerationEnvironment,
) -> Result<CommandPlan, CliError> {
    let result = runtime
        .run_agent_loop(
            StepInput::user_text(&command_generation_prompt(request, environment))
                .map_err(unexpected)?,
            StepContext::default(),
            command_generation_loop_config()?,
        )
        .await
        .map_err(unexpected)?;

    if !matches!(result.status(), AgentLoopStatus::Completed) {
        return Err(CliError::Unexpected(format!(
            "command generation did not complete: {:?}",
            result.status()
        )));
    }

    let final_output = result.final_output_json().ok_or_else(|| {
        CliError::Unexpected("command generation completed without structured output".to_owned())
    })?;
    serde_json::from_str::<CommandPlan>(final_output.json()).map_err(unexpected)
}

async fn write_command_plan_json<W>(plan: &CommandPlan, writer: W) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    let line = serde_json::to_string(plan).map_err(unexpected)?;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(stdout_error)?;
    writer.write_all(b"\n").await.map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)
}

async fn write_command_plan_summary<W>(plan: &CommandPlan, writer: W) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    writer
        .write_all(format!("command: {}\n", plan.shell_command).as_bytes())
        .await
        .map_err(stdout_error)?;
    if !plan.notes.is_empty() {
        writer.write_all(b"notes:\n").await.map_err(stdout_error)?;
        for note in &plan.notes {
            writer
                .write_all(format!("- {note}\n").as_bytes())
                .await
                .map_err(stdout_error)?;
        }
    }
    if !plan.cautions.is_empty() {
        writer
            .write_all(b"cautions:\n")
            .await
            .map_err(stdout_error)?;
        for caution in &plan.cautions {
            writer
                .write_all(format!("- {caution}\n").as_bytes())
                .await
                .map_err(stdout_error)?;
        }
    }
    writer.flush().await.map_err(stdout_error)
}

pub(crate) async fn prompt_execute_command_plan<R, W>(
    _plan: &CommandPlan,
    reader: R,
    writer: W,
) -> Result<bool, CliError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut writer = BufWriter::new(writer);
    writer
        .write_all(b"execute? [y/N] ")
        .await
        .map_err(stdout_error)?;
    writer.flush().await.map_err(stdout_error)?;

    let mut reader = reader;
    let mut answer = String::new();
    reader
        .read_line(&mut answer)
        .await
        .map_err(|error| CliError::Unexpected(error.to_string()))?;
    Ok(matches!(answer.trim(), "y" | "Y" | "yes" | "YES"))
}

pub(crate) async fn execute_shell_command_to_writer<W>(
    command: &str,
    writer: W,
) -> Result<(), CliError>
where
    W: AsyncWrite + Unpin,
{
    let mut child = TokioCommand::new("sh")
        .arg("-c")
        .arg(command)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(unexpected)?;

    let mut writer = BufWriter::new(writer);
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CliError::Unexpected("shell stdout pipe was not captured".to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| CliError::Unexpected("shell stderr pipe was not captured".to_owned()))?;

    let mut stdout = stdout;
    let mut stderr = stderr;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut stdout_buffer = vec![0_u8; 8192];
    let mut stderr_buffer = vec![0_u8; 8192];

    while stdout_open || stderr_open {
        tokio::select! {
            result = stdout.read(&mut stdout_buffer), if stdout_open => {
                let read = result.map_err(unexpected)?;
                if read == 0 {
                    stdout_open = false;
                } else {
                    writer
                        .write_all(&stdout_buffer[..read])
                        .await
                        .map_err(stdout_error)?;
                    writer.flush().await.map_err(stdout_error)?;
                }
            }
            result = stderr.read(&mut stderr_buffer), if stderr_open => {
                let read = result.map_err(unexpected)?;
                if read == 0 {
                    stderr_open = false;
                } else {
                    writer
                        .write_all(&stderr_buffer[..read])
                        .await
                        .map_err(stdout_error)?;
                    writer.flush().await.map_err(stdout_error)?;
                }
            }
        }
    }
    writer.flush().await.map_err(stdout_error)?;

    let status = child.wait().await.map_err(unexpected)?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::Unexpected(format!(
            "shell command exited with status {status}"
        )))
    }
}

fn cmd_check_command_tool(
    environment: CommandGenerationEnvironment,
) -> Result<RegisteredTool, CliError> {
    let schema = serde_json::from_value::<ToolInputSchema>(serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "programs": {
                "type": "array",
                "items": {
                    "type": "string",
                    "minLength": 1
                },
                "minItems": 1,
                "description": "Program names to check in PATH before suggesting optional system commands."
            }
        },
        "required": ["programs"]
    }))
    .map_err(unexpected)?;
    let spec = ToolSpec::new(
        ToolName::new(CHECK_COMMAND_TOOL_NAME).map_err(unexpected)?,
        "Check whether command names are available in this environment without executing them.",
        schema,
    )
    .map_err(unexpected)?;

    Ok(RegisteredTool::read_only(
        spec,
        Arc::new(CmdCheckCommandExecutor { environment }),
    ))
}

struct CmdCheckCommandExecutor {
    environment: CommandGenerationEnvironment,
}

impl ToolExecutor for CmdCheckCommandExecutor {
    fn execute<'a>(
        &'a self,
        call: PendingToolCall,
        _context: ToolExecutionContext,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let programs = match cmd_check_programs_from_call(&call) {
                Ok(programs) => programs,
                Err(message) => return Ok(cmd_check_invalid_arguments_outcome(message)),
            };
            let results = programs
                .iter()
                .map(|program| self.environment.command_availability(program))
                .collect::<Vec<_>>();
            let payload = serde_json::json!({
                "ok": true,
                "tool": CHECK_COMMAND_TOOL_NAME,
                "results": results
            });
            Ok(ToolExecutionOutcome::succeeded_json(payload.to_string()))
        })
    }
}

fn cmd_check_programs_from_call(call: &PendingToolCall) -> Result<Vec<String>, String> {
    let Some(value) = call.arguments().as_object().get("programs") else {
        return Err("cmd_check_command requires programs".to_owned());
    };
    let Some(values) = value.as_array() else {
        return Err("cmd_check_command programs must be an array".to_owned());
    };
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("cmd_check_command programs[{index}] must be a string"))
        })
        .collect()
}

fn cmd_check_invalid_arguments_outcome(message: String) -> ToolExecutionOutcome {
    let diagnostic = ErrorInfo::new(CHECK_COMMAND_INVALID_ARGUMENTS_CODE, &message)
        .expect("static cmd check diagnostic code must be valid");
    let payload = serde_json::json!({
        "ok": false,
        "tool": CHECK_COMMAND_TOOL_NAME,
        "error": message
    });
    ToolExecutionOutcome::failed_json(payload.to_string(), diagnostic)
}

#[derive(Serialize)]
struct CommandAvailability {
    program: String,
    available: bool,
    kind: &'static str,
    path: Option<String>,
}

fn is_shell_builtin(program: &str) -> bool {
    matches!(
        program,
        "cd" | "echo" | "eval" | "exec" | "exit" | "export" | "printf" | "pwd" | "read" | "test"
    )
}

fn find_program_in_path(program: &str, path: &str) -> Option<String> {
    if program.contains('/') || program.trim().is_empty() {
        return None;
    }
    env::split_paths(path).find_map(|directory| {
        let candidate = directory.join(program);
        if is_executable_file(&candidate) {
            Some(candidate.display().to_string())
        } else {
            None
        }
    })
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}
