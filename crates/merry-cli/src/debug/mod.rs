use clap::{Subcommand, ValueEnum};

pub(crate) mod basic;
pub(crate) mod coding_loop;
pub(crate) mod openai;
pub(crate) mod shell;

#[derive(Debug, clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct Args {
    #[arg(
        long,
        value_name = "SESSION_ID",
        default_value = crate::DEFAULT_SESSION_ID,
        allow_hyphen_values = true,
        help = "Session id to use"
    )]
    pub(crate) session_id: String,

    #[arg(
        long,
        value_name = "TEXT",
        default_value = crate::DEFAULT_INPUT,
        allow_hyphen_values = true,
        help = "User text input"
    )]
    pub(crate) input: String,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    #[command(
        name = "openai",
        about = "Run opt-in OpenAI-compatible model debugging",
        after_help = crate::OPENAI_ENV_HELP
    )]
    OpenAi(OpenAiArgs),
    #[command(
        name = "shell",
        about = "Run a local command through Merry's process action protocol"
    )]
    Shell(ShellArgs),
    #[command(
        name = "coding-loop-smoke",
        about = "Run an opt-in sandboxed coding-loop smoke with deterministic model steps"
    )]
    CodingLoopSmoke,
    #[command(
        name = "permission-network-smoke",
        about = "Run an opt-in sandboxed permission review smoke driven by a live OpenAI-compatible model",
        after_help = crate::OPENAI_ENV_HELP
    )]
    PermissionNetworkSmoke(PermissionNetworkSmokeArgs),
    #[command(
        name = "coding-loop-live-smoke",
        about = "Run an opt-in sandboxed coding-loop smoke driven by a live OpenAI-compatible model"
    )]
    CodingLoopLiveSmoke(CodingLoopLiveSmokeArgs),
    #[command(
        name = "coding-loop-task-smoke",
        about = "Run an opt-in sandboxed coding-loop task smoke with deterministic model steps"
    )]
    CodingLoopTaskSmoke(CodingLoopTaskSmokeArgs),
    #[command(
        name = "coding-loop-task-live-smoke",
        about = "Run an opt-in sandboxed coding-loop task smoke driven by a live OpenAI-compatible model"
    )]
    CodingLoopTaskLiveSmoke(CodingLoopTaskLiveSmokeArgs),
    #[command(
        name = "coding-loop-subagent-live-smoke",
        about = "Run an opt-in sandboxed coding-loop smoke that requires a live model to delegate to a child agent"
    )]
    CodingLoopSubagentLiveSmoke(CodingLoopSubagentLiveSmokeArgs),
}

#[derive(Debug, clap::Args)]
pub(crate) struct OpenAiArgs {
    #[arg(
        long,
        required = true,
        value_name = "TEXT",
        allow_hyphen_values = true,
        help = "User text input to send through Runtime::step"
    )]
    pub(crate) input: String,

    #[arg(
        long,
        value_name = "MODEL",
        allow_hyphen_values = true,
        help = "Model name; overrides [providers.default].model"
    )]
    pub(crate) model: Option<String>,

    #[arg(
        long,
        value_name = "N",
        value_parser = crate::parse_max_output_tokens,
        help = "Optional maximum output tokens for this step"
    )]
    pub(crate) max_output_tokens: Option<u64>,

    #[arg(
        long,
        value_name = "TEXT",
        allow_hyphen_values = true,
        help = "Require first step to call debug_echo; return this text"
    )]
    pub(crate) debug_tool_result: Option<String>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct ShellArgs {
    #[arg(
        long = "accept-local-workspace-process-risk",
        help = "Accept local workspace process risk when running inside Merry's sandbox handoff"
    )]
    pub(crate) accept_local_workspace_process_risk: bool,

    #[arg(
        long = "events-jsonl",
        help = "Print runtime lifecycle events as JSONL instead of command stdout/stderr"
    )]
    pub(crate) events_jsonl: bool,

    #[arg(
        required = true,
        allow_hyphen_values = true,
        last = true,
        num_args = 1..,
        value_name = "ARGV",
        help = "Command argv to run after `shell --`; no shell string parsing is performed"
    )]
    pub(crate) argv: Vec<String>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct CodingLoopLiveSmokeArgs {
    #[arg(
        long,
        value_name = "MODEL",
        allow_hyphen_values = true,
        help = "Model name; overrides [providers.default].model"
    )]
    pub(crate) model: Option<String>,

    #[arg(
        long,
        value_name = "N",
        value_parser = crate::parse_max_output_tokens,
        default_value_t = 512,
        help = "Maximum output tokens per live model step"
    )]
    pub(crate) max_output_tokens: u64,
}

#[derive(Debug, clap::Args)]
pub(crate) struct PermissionNetworkSmokeArgs {
    #[arg(
        long,
        value_name = "MODEL",
        allow_hyphen_values = true,
        help = "Model name; overrides [providers.default].model"
    )]
    pub(crate) model: Option<String>,

    #[arg(
        long,
        value_name = "N",
        value_parser = crate::parse_max_output_tokens,
        default_value_t = 768,
        help = "Maximum output tokens for each live model step"
    )]
    pub(crate) max_output_tokens: u64,
}

#[derive(Debug, clap::Args)]
pub(crate) struct CodingLoopTaskSmokeArgs {
    #[arg(
        long,
        value_enum,
        default_value = "status-text",
        help = "Disposable coding task fixture to run"
    )]
    pub(crate) task: CodingLoopTaskSmokeTask,
}

#[derive(Debug, clap::Args)]
pub(crate) struct CodingLoopTaskLiveSmokeArgs {
    #[arg(
        long,
        value_enum,
        default_value = "status-text",
        help = "Disposable coding task fixture to run"
    )]
    pub(crate) task: CodingLoopTaskSmokeTask,

    #[arg(
        long,
        value_name = "MODEL",
        allow_hyphen_values = true,
        help = "Model name; overrides [providers.default].model"
    )]
    pub(crate) model: Option<String>,

    #[arg(
        long,
        value_name = "N",
        value_parser = crate::parse_max_output_tokens,
        default_value_t = 768,
        help = "Maximum output tokens for each live model step"
    )]
    pub(crate) max_output_tokens: u64,
}

#[derive(Debug, clap::Args)]
pub(crate) struct CodingLoopSubagentLiveSmokeArgs {
    #[arg(
        long,
        value_name = "MODEL",
        allow_hyphen_values = true,
        help = "Model name; overrides [providers.default].model"
    )]
    pub(crate) model: Option<String>,

    #[arg(
        long,
        value_name = "N",
        value_parser = crate::parse_max_output_tokens,
        default_value_t = 768,
        help = "Maximum output tokens for each live model step"
    )]
    pub(crate) max_output_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum CodingLoopTaskSmokeTask {
    StatusText,
}
