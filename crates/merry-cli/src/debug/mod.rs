use clap::Subcommand;

pub(crate) mod basic;
pub(crate) mod openai;
pub(crate) mod shell;

pub(crate) const DEFAULT_SESSION_ID: &str = "debug-session";
pub(crate) const DEFAULT_INPUT: &str = "debug step";

#[derive(Debug, clap::Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct Args {
    #[arg(
        long,
        value_name = "SESSION_ID",
        default_value = DEFAULT_SESSION_ID,
        allow_hyphen_values = true,
        help = "Session id to use"
    )]
    pub(crate) session_id: String,

    #[arg(
        long,
        value_name = "TEXT",
        default_value = DEFAULT_INPUT,
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
        after_help = crate::cli::OPENAI_ENV_HELP
    )]
    OpenAi(OpenAiArgs),
    #[command(
        name = "shell",
        about = "Run a local command through Merry's process action protocol"
    )]
    Shell(ShellArgs),
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
        value_parser = crate::cli::parse_max_output_tokens,
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
