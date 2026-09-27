use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::size::{Cpu, Memory, SizeRequest};

/// ECS の使い捨てタスクを起動して入り、抜けたら止める。コマンドを流すこともできる
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

        #[command(flatten)]
        size: SizeArgs,
    },
    /// ecsh が起動したタスクのうち、動いているものを一覧する
    Ps {
        /// 設定ファイルの [profiles.<名前>]。省略すると一覧から選ぶ
        profile: Option<String>,

        /// 設定の全プロファイルを回って 1 つの表にまとめる
        #[arg(long, conflicts_with = "profile")]
        all: bool,
    },
    /// ecsh が起動して残ったタスクを、一覧から選んで止める
    Gc {
        /// 設定ファイルの [profiles.<名前>]。省略すると一覧から選ぶ
        profile: Option<String>,

        /// 設定の全プロファイルを回って 1 つの一覧にまとめる
        #[arg(long, conflicts_with = "profile")]
        all: bool,

        /// 一覧を出さず、止め忘れと不明のタスクを止める。run のタスクは止めない
        #[arg(short, long)]
        yes: bool,
    },
    /// コマンドを使い捨てタスクのコマンドとして流す。既定では終わるまで出力を流し、コマンドの終了コードで終わる
    Run {
        /// 設定ファイルの [profiles.<名前>]。省略すると一覧から選ぶ
        profile: Option<String>,

        /// 起動前の y/N の確認を省く
        #[arg(short, long)]
        yes: bool,

        /// 起動だけして抜ける。タスクは最後まで動く
        #[arg(short, long)]
        detach: bool,

        #[command(flatten)]
        size: SizeArgs,

        /// `--` の後ろに、流すコマンド。シェルを通さず、そのままコンテナのコマンドにする
        #[arg(last = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
    /// この Mac で run したものの履歴から選び、出力を見る。動いているものは終わるまで流す
    Logs {
        /// 設定ファイルの [profiles.<名前>]。省略すると一覧から選ぶ
        profile: Option<String>,

        /// 一覧を出さず、直近の 1 件を開く。プロファイルを省略したら、プロファイルを問わず直近
        #[arg(long)]
        last: bool,

        /// 出力を流す代わりに、CloudWatch Logs のページをブラウザで開く
        #[arg(long)]
        open: bool,
    },
    /// ecsh が起動して動いているタスクを選び、タスクの詳細かログを AWS コンソールで開く
    Open {
        /// 設定ファイルの [profiles.<名前>]。省略すると一覧から選ぶ
        profile: Option<String>,

        /// 選ばせずにタスクの詳細を開く
        #[arg(long, conflicts_with = "logs")]
        task: bool,

        /// 選ばせずにログ（CloudWatch Logs）を開く。run のタスクだけ
        #[arg(long)]
        logs: bool,
    },
}

/// exec / run のタスクの大きさ。何も付けなければタスク定義のまま
#[derive(Debug, Args)]
pub struct SizeArgs {
    /// タスクの大きさ（CPU / メモリ）を一覧から選ぶ。先頭はタスク定義のまま
    #[arg(long, conflicts_with_all = ["yes", "cpu", "memory"])]
    size: bool,

    /// タスクの CPU（vCPU）。例: 2、0.5。省略するとタスク定義の値
    #[arg(long, value_name = "VCPU")]
    cpu: Option<Cpu>,

    /// タスクのメモリ。例: 8GB、512MB。省略するとタスク定義の値
    #[arg(long, value_name = "SIZE")]
    memory: Option<Memory>,
}

impl SizeArgs {
    pub fn request(&self) -> SizeRequest {
        match self {
            Self { size: true, .. } => SizeRequest::Choose,
            Self {
                cpu: None,
                memory: None,
                ..
            } => SizeRequest::Keep,
            &Self { cpu, memory, .. } => SizeRequest::Custom { cpu, memory },
        }
    }
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
                | Command::Ps { profile, .. }
                | Command::Gc { profile, .. } => profile,
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
    fn ps_does_not_accept_yes() {
        assert!(Cli::try_parse_from(["ecsh", "ps", "--yes"]).is_err());
    }

    #[test]
    fn gc_accepts_all_and_yes_together_but_not_all_with_a_profile() {
        let parse = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Command::Gc { profile, all, yes } => (profile, all, yes),
            command => panic!("gc として解釈されない: {command:?}"),
        };

        assert_eq!(
            parse(&["ecsh", "gc", "staging"]),
            (Some("staging".into()), false, false)
        );
        assert_eq!(
            parse(&["ecsh", "gc", "-y", "staging"]),
            (Some("staging".into()), false, true)
        );
        assert_eq!(parse(&["ecsh", "gc", "--all", "--yes"]), (None, true, true));
        assert!(Cli::try_parse_from(["ecsh", "gc", "staging", "--all"]).is_err());
    }

    #[test]
    fn ps_all_lists_every_profile_instead_of_a_named_one() {
        let parse_all = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Command::Ps { profile, all } => (profile, all),
            command => panic!("ps として解釈されない: {command:?}"),
        };

        assert_eq!(parse_all(&["ecsh", "ps", "--all"]), (None, true));
        assert_eq!(
            parse_all(&["ecsh", "ps", "staging"]),
            (Some("staging".into()), false)
        );
        assert!(Cli::try_parse_from(["ecsh", "ps", "staging", "--all"]).is_err());
    }

    fn parse_run(args: &[&str]) -> (Option<String>, bool, bool, Vec<String>) {
        match Cli::try_parse_from(args).unwrap().command {
            Command::Run {
                profile,
                yes,
                detach,
                command,
                ..
            } => (profile, yes, detach, command),
            command => panic!("run として解釈されない: {command:?}"),
        }
    }

    #[test]
    fn run_takes_everything_after_double_dash_as_the_command_verbatim() {
        let (profile, yes, detach, command) = parse_run(&[
            "ecsh",
            "run",
            "staging",
            "--",
            "bundle",
            "exec",
            "rake",
            "-y",
            "--detach",
            "task[a, b]",
        ]);

        assert_eq!(profile.as_deref(), Some("staging"));
        assert!(!yes);
        assert!(!detach);
        assert_eq!(
            command,
            ["bundle", "exec", "rake", "-y", "--detach", "task[a, b]"]
        );
    }

    #[test]
    fn run_accepts_yes_and_detach_before_double_dash() {
        let (_, yes, detach, _) = parse_run(&["ecsh", "run", "-y", "-d", "staging", "--", "true"]);
        assert!(yes && detach);

        let (_, yes, detach, _) =
            parse_run(&["ecsh", "run", "staging", "--yes", "--detach", "--", "true"]);
        assert!(yes && detach);
    }

    #[test]
    fn run_profile_can_be_omitted_before_double_dash() {
        let (profile, _, _, command) =
            parse_run(&["ecsh", "run", "--", "rake", "db:migrate:status"]);

        assert_eq!(profile, None);
        assert_eq!(command, ["rake", "db:migrate:status"]);
    }

    #[test]
    fn run_without_a_command_is_parsed_so_that_it_can_point_to_exec() {
        assert_eq!(
            parse_run(&["ecsh", "run", "staging"]).3,
            Vec::<String>::new()
        );
        assert_eq!(
            parse_run(&["ecsh", "run", "staging", "--"]).3,
            Vec::<String>::new()
        );
    }

    fn parse_logs(args: &[&str]) -> (Option<String>, bool) {
        match Cli::try_parse_from(args).unwrap().command {
            Command::Logs { profile, last, .. } => (profile, last),
            command => panic!("logs として解釈されない: {command:?}"),
        }
    }

    #[test]
    fn logs_profile_can_be_omitted_with_or_without_last() {
        assert_eq!(parse_logs(&["ecsh", "logs"]), (None, false));
        assert_eq!(parse_logs(&["ecsh", "logs", "--last"]), (None, true));
        assert_eq!(
            parse_logs(&["ecsh", "logs", "staging", "--last"]),
            (Some("staging".into()), true)
        );
    }

    #[test]
    fn logs_open_is_accepted_with_or_without_last() {
        let parse_open = |args: &[&str]| match Cli::try_parse_from(args).unwrap().command {
            Command::Logs { last, open, .. } => (last, open),
            command => panic!("logs として解釈されない: {command:?}"),
        };

        assert_eq!(parse_open(&["ecsh", "logs"]), (false, false));
        assert_eq!(parse_open(&["ecsh", "logs", "--open"]), (false, true));
        assert_eq!(
            parse_open(&["ecsh", "logs", "staging", "--last", "--open"]),
            (true, true)
        );
    }

    fn parse_open(args: &[&str]) -> (Option<String>, bool, bool) {
        match Cli::try_parse_from(args).unwrap().command {
            Command::Open {
                profile,
                task,
                logs,
            } => (profile, task, logs),
            command => panic!("open として解釈されない: {command:?}"),
        }
    }

    #[test]
    fn open_profile_can_be_omitted_and_task_or_logs_skips_the_page_choice() {
        assert_eq!(parse_open(&["ecsh", "open"]), (None, false, false));
        assert_eq!(
            parse_open(&["ecsh", "open", "staging", "--task"]),
            (Some("staging".into()), true, false)
        );
        assert_eq!(parse_open(&["ecsh", "open", "--logs"]), (None, false, true));
    }

    #[test]
    fn open_does_not_accept_both_task_and_logs_nor_all() {
        assert!(Cli::try_parse_from(["ecsh", "open", "--task", "--logs"]).is_err());
        assert!(Cli::try_parse_from(["ecsh", "open", "--all"]).is_err());
    }

    fn size_request(args: &[&str]) -> SizeRequest {
        match Cli::try_parse_from(args).unwrap().command {
            Command::Exec { size, .. } | Command::Run { size, .. } => size.request(),
            command => panic!("exec / run として解釈されない: {command:?}"),
        }
    }

    #[test]
    fn exec_and_run_keep_the_task_definition_size_by_default() {
        assert_eq!(
            size_request(&["ecsh", "exec", "staging"]),
            SizeRequest::Keep
        );
        assert_eq!(
            size_request(&["ecsh", "run", "staging", "--", "true"]),
            SizeRequest::Keep
        );
    }

    #[test]
    fn size_flag_chooses_from_the_list_on_exec_and_run() {
        assert_eq!(
            size_request(&["ecsh", "exec", "--size"]),
            SizeRequest::Choose
        );
        assert_eq!(
            size_request(&["ecsh", "run", "--size", "--", "true"]),
            SizeRequest::Choose
        );
    }

    #[test]
    fn cpu_and_memory_are_given_in_human_units_and_either_can_be_omitted() {
        assert_eq!(
            size_request(&["ecsh", "exec", "--cpu", "2", "--memory", "8GB"]),
            SizeRequest::Custom {
                cpu: Some(Cpu::from_units(2048)),
                memory: Some(Memory::from_mib(8192)),
            }
        );
        assert_eq!(
            size_request(&["ecsh", "run", "--memory", "512MB", "--", "true"]),
            SizeRequest::Custom {
                cpu: None,
                memory: Some(Memory::from_mib(512)),
            }
        );
    }

    #[test]
    fn unreadable_cpu_or_memory_is_rejected_before_anything_runs() {
        assert!(Cli::try_parse_from(["ecsh", "exec", "--cpu", "two"]).is_err());
        assert!(Cli::try_parse_from(["ecsh", "exec", "--memory", "8"]).is_err());
    }

    #[test]
    fn size_cannot_be_combined_with_yes_cpu_or_memory() {
        for args in [
            ["ecsh", "exec", "--size", "-y"].as_slice(),
            &["ecsh", "exec", "--size", "--cpu", "2"],
            &["ecsh", "exec", "--size", "--memory", "8GB"],
            &["ecsh", "run", "--size", "--yes", "--", "true"],
        ] {
            assert!(Cli::try_parse_from(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn cpu_and_memory_can_be_combined_with_yes() {
        assert!(
            Cli::try_parse_from(["ecsh", "exec", "-y", "--cpu", "2", "--memory", "8GB"]).is_ok()
        );
    }

    #[test]
    fn run_command_without_double_dash_is_rejected() {
        assert!(Cli::try_parse_from(["ecsh", "run", "staging", "rake"]).is_err());
    }
}
