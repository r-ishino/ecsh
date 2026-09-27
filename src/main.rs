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
mod session_lock;
mod signals;
mod ui;
mod xdg;

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
    if let Command::Run { args } = &cli.command {
        return commands::run(args);
    }
    let config_path = match cli.config {
        Some(path) => path,
        None => config::default_path()?,
    };
    let config = Config::load(&config_path)?;

    match cli.command {
        Command::Exec { profile, yes } => {
            let (name, profile) = prompt::select_profile(&config, profile.as_deref())?;
            commands::exec(name, profile, yes).await
        }
        Command::Ps { all: true, .. } => commands::ps_all(&config).await,
        Command::Ps { profile, .. } => {
            let (name, profile) = prompt::select_profile(&config, profile.as_deref())?;
            commands::ps(name, profile).await
        }
        Command::Gc { all: true, yes, .. } => commands::gc_all(&config, yes).await,
        Command::Gc { profile, yes, .. } => {
            let (name, profile) = prompt::select_profile(&config, profile.as_deref())?;
            commands::gc(name, profile, yes).await
        }
        Command::Run { .. } => unreachable!("設定を読む前に案内して終えている"),
    }
}
