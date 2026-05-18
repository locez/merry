//! Debug and demonstration CLI for Merry.

use futures_util::StreamExt;
use merry_core::SessionId;
use merry_runtime::{Runtime, StepContext, StepInput};
use std::{
    env, fmt, io,
    process::{ExitCode, Termination},
};
use tokio::io::{AsyncWriteExt, BufWriter};

const DEFAULT_SESSION_ID: &str = "debug-session";
const DEFAULT_INPUT: &str = "debug step";

fn main() -> CliExit {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return CliExit::Unexpected(err.to_string()),
    };

    runtime.block_on(async_main())
}

async fn async_main() -> CliExit {
    match parse_args(env::args().skip(1)) {
        Ok(Command::Help) => {
            print!("{}", root_usage());
            CliExit::Success
        }
        Ok(Command::DebugHelp) => {
            print!("{}", debug_usage());
            CliExit::Success
        }
        Ok(Command::Debug { session_id, input }) => match run_debug(&session_id, &input).await {
            Ok(()) => CliExit::Success,
            Err(CliError::BrokenPipe) => CliExit::Success,
            Err(CliError::DebugUsage(message)) => CliExit::Usage {
                message,
                usage: debug_usage(),
            },
            Err(CliError::Unexpected(message)) => CliExit::Unexpected(message),
        },
        Err(ParseError::Root(message)) => CliExit::Usage {
            message,
            usage: root_usage(),
        },
        Err(ParseError::Debug(message)) => CliExit::Usage {
            message,
            usage: debug_usage(),
        },
    }
}

enum Command {
    Help,
    DebugHelp,
    Debug { session_id: String, input: String },
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Command, ParseError> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Err(ParseError::Root("missing command".to_owned()));
    };

    match command.as_str() {
        "--help" => {
            if let Some(extra) = args.next() {
                return Err(ParseError::Root(format!(
                    "unexpected argument after --help: {extra}"
                )));
            }
            Ok(Command::Help)
        }
        "debug" => parse_debug_args(args),
        other => Err(ParseError::Root(format!("unknown command: {other}"))),
    }
}

fn parse_debug_args(args: impl IntoIterator<Item = String>) -> Result<Command, ParseError> {
    let mut session_id = DEFAULT_SESSION_ID.to_owned();
    let mut input = DEFAULT_INPUT.to_owned();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" => {
                if let Some(extra) = args.next() {
                    return Err(ParseError::Debug(format!(
                        "unexpected argument after debug --help: {extra}"
                    )));
                }
                return Ok(Command::DebugHelp);
            }
            "--session-id" => {
                session_id = args
                    .next()
                    .ok_or_else(|| ParseError::Debug("--session-id requires a value".to_owned()))?;
            }
            "--input" => {
                input = args
                    .next()
                    .ok_or_else(|| ParseError::Debug("--input requires a value".to_owned()))?;
            }
            other if other.starts_with("--") => {
                return Err(ParseError::Debug(format!("unknown debug option: {other}")));
            }
            other => {
                return Err(ParseError::Debug(format!(
                    "unexpected debug argument: {other}"
                )));
            }
        }
    }

    Ok(Command::Debug { session_id, input })
}

async fn run_debug(session_id: &str, input: &str) -> Result<(), CliError> {
    let session_id = SessionId::new(session_id).map_err(usage_error)?;
    let runtime = Runtime::builder(session_id).build().map_err(unexpected)?;
    let input = StepInput::user_text(input).map_err(usage_error)?;
    let mut events = runtime
        .step(input, StepContext::default())
        .map_err(unexpected)?;
    let stdout = tokio::io::stdout();
    let mut writer = BufWriter::new(stdout);

    while let Some(event) = events.next().await {
        let line = serde_json::to_string(&event).map_err(unexpected)?;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(stdout_error)?;
        writer.write_all(b"\n").await.map_err(stdout_error)?;
    }

    writer.flush().await.map_err(stdout_error)
}

fn root_usage() -> &'static str {
    "Usage: merry <COMMAND>\n\nCommands:\n  debug    Print deterministic runtime events as JSON lines\n\nOptions:\n  --help   Print help\n"
}

fn debug_usage() -> &'static str {
    "Usage: merry debug [--session-id <SESSION_ID>] [--input <TEXT>]\n\nOptions:\n  --session-id <SESSION_ID>   Session id to use [default: debug-session]\n  --input <TEXT>              User text input [default: debug step]\n  --help                      Print help\n"
}

fn unexpected(err: impl fmt::Display) -> CliError {
    CliError::Unexpected(err.to_string())
}

fn usage_error(err: impl fmt::Display) -> CliError {
    CliError::DebugUsage(err.to_string())
}

fn stdout_error(err: io::Error) -> CliError {
    if err.kind() == io::ErrorKind::BrokenPipe {
        CliError::BrokenPipe
    } else {
        CliError::Unexpected(err.to_string())
    }
}

enum CliError {
    BrokenPipe,
    DebugUsage(String),
    Unexpected(String),
}

enum ParseError {
    Root(String),
    Debug(String),
}

enum CliExit {
    Success,
    Usage {
        message: String,
        usage: &'static str,
    },
    Unexpected(String),
}

impl Termination for CliExit {
    fn report(self) -> ExitCode {
        match self {
            Self::Success => ExitCode::SUCCESS,
            Self::Usage { message, usage } => {
                eprintln!("{message}\n\n{usage}");
                ExitCode::from(2)
            }
            Self::Unexpected(message) => {
                eprintln!("{message}");
                ExitCode::FAILURE
            }
        }
    }
}
