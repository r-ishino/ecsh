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
    Exec {
        /// 設定ファイルの [profiles.<名前>]。省略すると一覧から選ぶ
        profile: Option<String>,

        /// 起動前の y/N の確認を省く
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
    /// 旧名の `run` で入ろうとした手を止め、`exec` を案内する。コマンドを流す `run` を作るまでの仮置き
    #[command(hide = true)]
    Run {
        /// 旧 `run` の引数。何が付いていても解釈せず、案内に使うだけ
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
            Cli::try_parse_from(["ecsh", "exec", "staging", "--config", "/tmp/c.toml"]).unwrap();

        assert_eq!(cli.config, Some(PathBuf::from("/tmp/c.toml")));
        assert!(
            matches!(cli.command, Command::Exec { profile, .. } if profile.as_deref() == Some("staging"))
        );
    }

    #[test]
    fn profile_can_be_omitted() {
        for subcommand in ["exec", "ps", "gc"] {
            let cli = Cli::try_parse_from(["ecsh", subcommand]).unwrap();

            let profile = match cli.command {
                Command::Exec { profile, .. }
                | Command::Ps { profile }
                | Command::Gc { profile } => profile,
                command => panic!("{subcommand} として解釈されない: {command:?}"),
            };
            assert_eq!(profile, None, "{subcommand}");
        }
    }

    #[test]
    fn exec_asks_for_confirmation_unless_yes_is_given() {
        let parse_yes = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Command::Exec { yes, .. } => yes,
            command => panic!("exec として解釈されない: {command:?}"),
        };

        assert!(!parse_yes(&["ecsh", "exec", "production"]));
        assert!(parse_yes(&["ecsh", "exec", "production", "--yes"]));
        assert!(parse_yes(&["ecsh", "exec", "-y", "production"]));
    }

    #[test]
    fn yes_is_only_accepted_by_exec() {
        assert!(Cli::try_parse_from(["ecsh", "ps", "--yes"]).is_err());
        assert!(Cli::try_parse_from(["ecsh", "gc", "-y"]).is_err());
    }

    #[test]
    fn run_accepts_any_arguments_of_the_former_run() {
        let parse_args = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Command::Run { args } => args,
            command => panic!("run として解釈されない: {command:?}"),
        };

        assert_eq!(parse_args(&["ecsh", "run"]), Vec::<String>::new());
        assert_eq!(parse_args(&["ecsh", "run", "staging"]), ["staging"]);
        assert_eq!(
            parse_args(&["ecsh", "run", "-y", "staging"]),
            ["-y", "staging"]
        );
        assert_eq!(
            parse_args(&["ecsh", "run", "staging", "--", "rake", "db:migrate"]),
            ["staging", "--", "rake", "db:migrate"]
        );
    }

    #[test]
    fn run_is_hidden_from_help() {
        let help = Cli::command().render_help().to_string();

        assert!(help.contains("exec"), "{help}");
        assert!(!help.contains("run"), "{help}");
    }
}
