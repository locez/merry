//! Merry terminal client and headless agent entrypoint.

mod cli;
mod cli_error;
mod cli_exit;
mod cli_route;
mod cmd;
mod coding;
mod completions;
mod config;
mod debug;
mod headless_review;
mod mcp_tools;
mod observability;
mod provider_config;
mod provider_management;
mod run;
mod runtime_config;
mod runtime_events;
mod sandbox;
mod session_id;
mod testing;
mod tool_display;
mod tui;
mod web;

use clap::Parser;
use coding::ProcessExecutionMode;
use config::{MerryConfig, XdgPaths};
use std::{env, io};

use cli::Cli;
use cli_exit::CliExit;
use runtime_config::{effective_log_settings, validate_loaded_config};

fn main() -> CliExit {
    let argv = env::args_os().collect::<Vec<_>>();
    let mut cli = match Cli::try_parse_from(argv.clone()) {
        Ok(cli) => cli,
        Err(error) => return CliExit::Clap(error),
    };

    // Completion scripts come straight from the clap definition; they must
    // print even when no config exists yet or the current one fails to load.
    if let Some(cli::CliCommand::Completions(args)) = &cli.command {
        completions::write_completions(args.shell, &mut io::stdout().lock());
        return CliExit::Success;
    }

    let config_paths = match XdgPaths::from_env() {
        Ok(paths) => paths,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };
    let _config = match MerryConfig::load_optional(&config_paths) {
        Ok(config) => config,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };
    if let Err(error) = validate_loaded_config(_config.as_ref(), &config_paths) {
        return CliExit::Unexpected(error.to_string());
    }
    if let Some(config) = _config.as_ref() {
        cli.apply_defaults(config.cli_defaults());
    }
    let log_settings = match effective_log_settings(_config.as_ref(), &config_paths) {
        Ok(settings) => settings,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };

    let process_execution_mode = cli.process_execution_mode();
    if process_execution_mode.uses_inner_sandbox()
        && cli.is_product_surface()
        && let Err(error) = sandbox::ensure_bubblewrap_available()
    {
        return CliExit::Unexpected(error.to_string());
    }

    // Probe nested bubblewrap before the outer re-exec. Merry never downgrades
    // the sandbox mode on its own: when the host cannot nest, startup stops
    // with a diagnostic that explains the host setup and the explicit flags.
    let inside_sandbox =
        env::var_os(sandbox::MERRY_SANDBOX_ENV).as_deref() == Some(std::ffi::OsStr::new("1"));
    if cli.should_bootstrap_sandbox()
        && !inside_sandbox
        && let Err(error) = sandbox::ensure_nested_sandbox_available()
    {
        return CliExit::Unexpected(error.to_string());
    }

    // Resolved on the host, after the probe has settled that this invocation
    // really re-execs: the sandboxed child cannot open /dev/tty itself.
    let review_terminal = if cli.hands_off_review_terminal() {
        sandbox::ReviewTerminalHandoff::resolve_controlling_terminal()
    } else {
        None
    };
    if let Err(error) = sandbox::maybe_reexec(
        cli.should_bootstrap_sandbox(),
        cli.clipboard_access(),
        review_terminal,
        argv.iter().skip(1).cloned().collect(),
    ) {
        return CliExit::Unexpected(error.to_string());
    }

    if process_execution_mode.uses_inner_sandbox()
        && cli.is_product_surface()
        && (process_execution_mode == ProcessExecutionMode::InnerOnly || inside_sandbox)
        && let Err(error) = sandbox::ensure_inner_sandbox_available()
    {
        return CliExit::Unexpected(error.to_string());
    }

    let _observability_guard = match observability::init_observability(log_settings.as_ref()) {
        Ok(guard) => guard,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => return CliExit::Unexpected(err.to_string()),
    };

    runtime.block_on(cli_route::run(cli, _config))
}
