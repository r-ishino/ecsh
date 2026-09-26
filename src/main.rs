mod cli;
mod commands;
mod config;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::config::Config;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = match cli.config {
        Some(path) => path,
        None => config::default_path()?,
    };
    let config = Config::load(&config_path)?;

    match cli.command {
        Command::Run { profile } => commands::run(config.profile(&profile)?),
        Command::Ps { profile } => commands::ps(config.profile(&profile)?),
        Command::Gc { profile } => commands::gc(config.profile(&profile)?),
    }
}
