use std::process::ExitCode;

use clap::Parser;
use myforge_agent::app::{Cli, WebSocketConnector, dispatch};
use myforge_agent::config::AgentConfig;
use myforge_agent::logging::init_logging;
use myforge_agent::preflight::{SystemCapabilityProbe, run_preflight};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    let _ = dotenvy::dotenv();

    let config = match AgentConfig::from_process() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = init_logging(config.logging()) {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }

    let preflight = match run_preflight(&config, &SystemCapabilityProbe) {
        Ok(report) => report,
        Err(error) => {
            tracing::error!(error_code = error.code().as_str(), error = %error, "preflight failed");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = dispatch(cli.intent(), &config, &preflight, &WebSocketConnector).await {
        tracing::error!(error_code = error.code().as_str(), error = %error, "agent stopped");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
