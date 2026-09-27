mod aws_profile;
mod cli;
mod commands;
mod config;
mod ecs;
mod prompt;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::config::Config;
use crate::prompt::Abort;

#[tokio::main]
async fn main() -> ExitCode {
    let Err(error) = try_main().await else {
        return ExitCode::SUCCESS;
    };
    match error.downcast_ref::<Abort>() {
        Some(abort) => {
            if let Some(message) = abort.message() {
                eprintln!("{message}");
            }
            ExitCode::from(abort.exit_code())
        }
        None => {
            eprintln!("Error: {error:?}");
            ExitCode::FAILURE
        }
    }
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
