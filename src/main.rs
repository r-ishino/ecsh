mod agent_wait;
mod aws_error;
mod aws_profile;
mod cli;
mod commands;
mod config;
mod ecs;
mod prompt;
mod report;
mod session;
mod signals;
mod ui;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::commands::StopAbandoned;
use crate::config::Config;
use crate::prompt::Abort;
use crate::report::report;
use crate::signals::Interruption;
use crate::ui::Style;

#[tokio::main]
async fn main() -> ExitCode {
    let Err(error) = try_main().await else {
        return ExitCode::SUCCESS;
    };
    if let Some(abort) = error.downcast_ref::<Abort>() {
        if let Some(message) = abort.message() {
            report!("{message}");
        }
        return ExitCode::from(abort.exit_code());
    }
    if let Some(interruption) = error.downcast_ref::<Interruption>() {
        return ExitCode::from(interruption.signal.exit_code());
    }
    if let Some(abandoned) = error.downcast_ref::<StopAbandoned>() {
        report!("{}", Style::current().warning(abandoned));
        return ExitCode::from(abandoned.signal.exit_code());
    }
    ui::report_error(&error);
    ExitCode::FAILURE
}

async fn try_main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = match cli.config {
        Some(path) => path,
        None => config::default_path()?,
    };
    let config = Config::load(&config_path)?;

    match cli.command {
        Command::Run { profile, yes } => {
            let (name, profile) = prompt::select_profile(&config, profile.as_deref())?;
            commands::run(name, profile, yes).await
        }
        Command::Ps { profile } => {
            commands::ps(prompt::select_profile(&config, profile.as_deref())?.1)
        }
        Command::Gc { profile } => {
            commands::gc(prompt::select_profile(&config, profile.as_deref())?.1)
        }
    }
}
