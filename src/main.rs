mod agent_wait;
mod aws_error;
mod aws_profile;
mod cli;
mod commands;
mod config;
mod console;
mod ecs;
mod history;
mod logs;
mod prompt;
mod report;
mod session;
mod session_lock;
mod signals;
mod size;
mod ui;
mod xdg;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::commands::{Page, StopAbandoned};
use crate::config::Config;
use crate::prompt::Abort;
use crate::report::report;
use crate::signals::Interruption;
use crate::ui::Style;

#[tokio::main]
async fn main() -> ExitCode {
    let error = match try_main().await {
        Ok(code) => return code,
        Err(error) => error,
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

async fn try_main() -> Result<ExitCode> {
    let cli = Cli::parse();
    if let Command::Run {
        profile, command, ..
    } = &cli.command
        && command.is_empty()
    {
        return Err(commands::missing_command(profile.as_deref()));
    }
    let config_path = match cli.config {
        Some(path) => path,
        None => config::default_path()?,
    };
    let config = Config::load(&config_path)?;

    match cli.command {
        Command::Exec { profile, yes, size } => {
            let (name, profile) = prompt::select_profile(&config, profile.as_deref())?;
            commands::exec(name, profile, yes, size.request()).await?;
        }
        Command::Run {
            profile,
            yes,
            detach,
            command,
            size,
        } => {
            let (name, profile) = prompt::select_profile(&config, profile.as_deref())?;
            let code = commands::run(name, profile, &command, yes, detach, size.request()).await?;
            return Ok(ExitCode::from(code));
        }
        Command::Ps { all: true, .. } => commands::ps_all(&config).await?,
        Command::Ps { profile, .. } => {
            let (name, profile) = prompt::select_profile(&config, profile.as_deref())?;
            commands::ps(name, profile).await?;
        }
        Command::Gc { all: true, yes, .. } => commands::gc_all(&config, yes).await?,
        Command::Gc { profile, yes, .. } => {
            let (name, profile) = prompt::select_profile(&config, profile.as_deref())?;
            commands::gc(name, profile, yes).await?;
        }
        Command::Logs {
            profile,
            last,
            open,
        } => {
            let code = commands::logs(&config, profile.as_deref(), last, open).await?;
            return Ok(ExitCode::from(code));
        }
        Command::Open {
            profile,
            task,
            logs,
        } => {
            let (name, profile) = prompt::select_profile(&config, profile.as_deref())?;
            commands::open(name, profile, Page::from_flags(task, logs)).await?;
        }
    }
    Ok(ExitCode::SUCCESS)
}
