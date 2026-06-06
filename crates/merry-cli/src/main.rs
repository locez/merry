//! Debug and demonstration CLI for Merry.

mod cli;
mod cli_error;
mod cli_exit;
mod cli_route;
mod cmd;
mod coding_runtime;
mod config;
mod debug;
mod observability;
mod provider_config;
mod run;
mod runtime_config;
mod runtime_events;
mod sandbox;
mod testing;

use clap::Parser;
use config::{MerryConfig, XdgPaths};
use std::env;

use cli::Cli;
use cli_exit::CliExit;
use runtime_config::{effective_log_settings, validate_loaded_config};

fn main() -> CliExit {
    let argv = env::args_os().collect::<Vec<_>>();
    let cli = match Cli::try_parse_from(argv.clone()) {
        Ok(cli) => cli,
        Err(error) => return CliExit::Clap(error),
    };

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
    let log_settings = match effective_log_settings(_config.as_ref(), &config_paths) {
        Ok(settings) => settings,
        Err(error) => return CliExit::Unexpected(error.to_string()),
    };

    if let Err(error) =
        sandbox::maybe_reexec(cli.with_sandbox, argv.iter().skip(1).cloned().collect())
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
