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
        /// 設定ファイルの [profiles.<名前>]。省略すると一覧から選ぶ
        profile: Option<String>,

        /// confirm = true のプロファイルでも、起動前の確認を省く
        #[arg(short, long)]
        yes: bool,
    },
    /// ecsh が起動したタスクのうち、動いているものを一覧する
    Ps {
        /// 設定ファイルの [profiles.<名前>]。省略すると一覧から選ぶ
        profile: Option<String>,
    },
    /// 残ってしまった ecsh のタスクを止める
    Gc {
        /// 設定ファイルの [profiles.<名前>]。省略すると一覧から選ぶ
        profile: Option<String>,
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
        assert!(
            matches!(cli.command, Command::Run { profile, .. } if profile.as_deref() == Some("staging"))
        );
    }

    #[test]
    fn profile_can_be_omitted() {
        for subcommand in ["run", "ps", "gc"] {
            let cli = Cli::try_parse_from(["ecsh", subcommand]).unwrap();

            let profile = match cli.command {
                Command::Run { profile, .. }
                | Command::Ps { profile }
                | Command::Gc { profile } => profile,
            };
            assert_eq!(profile, None, "{subcommand}");
        }
    }

    #[test]
    fn run_asks_for_confirmation_unless_yes_is_given() {
        let parse_yes = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Command::Run { yes, .. } => yes,
            command => panic!("run として解釈されない: {command:?}"),
        };

        assert!(!parse_yes(&["ecsh", "run", "production"]));
        assert!(parse_yes(&["ecsh", "run", "production", "--yes"]));
        assert!(parse_yes(&["ecsh", "run", "-y", "production"]));
    }

    #[test]
    fn yes_is_only_accepted_by_run() {
        assert!(Cli::try_parse_from(["ecsh", "ps", "--yes"]).is_err());
        assert!(Cli::try_parse_from(["ecsh", "gc", "-y"]).is_err());
    }
}
