use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// ECS の使い捨てタスクを起動して入り、抜けたら止める
#[derive(Debug, Parser)]
#[command(version, about)]
pub struct Cli {
    /// 設定ファイルのパス [既定: $XDG_CONFIG_HOME/ecsh/config.toml または ~/.config/ecsh/config.toml]
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 使い捨てタスクを起動し、exec できるようになったら入る。抜けたらタスクを止める
    Run {
        /// 設定ファイルの [profiles.<名前>]
        profile: String,
    },
    /// ecsh が起動したタスクのうち、動いているものを一覧する
    Ps {
        /// 設定ファイルの [profiles.<名前>]
        profile: String,
    },
    /// 残ってしまった ecsh のタスクを止める
    Gc {
        /// 設定ファイルの [profiles.<名前>]
        profile: String,
    },
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn config_option_is_accepted_after_subcommand() {
        let cli =
            Cli::try_parse_from(["ecsh", "run", "staging", "--config", "/tmp/c.toml"]).unwrap();

        assert_eq!(cli.config, Some(PathBuf::from("/tmp/c.toml")));
        assert!(matches!(cli.command, Command::Run { profile } if profile == "staging"));
    }

    #[test]
    fn subcommand_requires_profile() {
        assert!(Cli::try_parse_from(["ecsh", "run"]).is_err());
    }
}
