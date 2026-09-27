use std::fmt;
use std::io::{self, IsTerminal, Write};

use anyhow::{Context, Result, bail};
use inquire::{InquireError, MultiSelect, Select};

use crate::config::{Config, Profile};
use crate::size::{self, DefinedSize, TaskSize};

/// 利用者が一覧や確認で操作を取りやめた
#[derive(Debug, PartialEq, Eq)]
pub enum Abort {
    /// Esc や N で断った
    Cancelled(&'static str),
    /// 一覧の選択中に Ctrl-C を押した
    Interrupted,
}

impl Abort {
    pub fn message(&self) -> Option<&'static str> {
        match self {
            Self::Cancelled(message) => Some(message),
            Self::Interrupted => None,
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Cancelled(_) => 1,
            Self::Interrupted => 130,
        }
    }
}

impl fmt::Display for Abort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message().unwrap_or("中断しました"))
    }
}

impl std::error::Error for Abort {}

/// 利用者とのやりとり。テストでは差し替える
pub trait Console {
    fn stdin_is_terminal(&self) -> bool;
    /// items から 1 つ選ばせて添字を返す。Esc で取りやめたら None
    fn select(&mut self, message: &str, items: Vec<String>) -> Result<Option<usize>>;
    /// items から複数選ばせて添字を返す。defaults の添字は最初から選んである。Esc で取りやめたら None
    fn multi_select(
        &mut self,
        message: &str,
        items: Vec<String>,
        defaults: &[usize],
    ) -> Result<Option<Vec<usize>>>;
    /// question を出して 1 行読む。入力が終わっていれば空文字列
    fn read_line(&mut self, question: &str) -> Result<String>;
}

pub struct Terminal;

impl Console for Terminal {
    fn stdin_is_terminal(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn select(&mut self, message: &str, items: Vec<String>) -> Result<Option<usize>> {
        let selected = Select::new(message, items)
            .with_help_message("↑↓ で移動、Enter で決定、文字を打つと絞り込み")
            .raw_prompt();
        match selected {
            Ok(option) => Ok(Some(option.index)),
            Err(InquireError::OperationCanceled) => Ok(None),
            Err(InquireError::OperationInterrupted) => Err(Abort::Interrupted.into()),
            Err(error) => Err(error.into()),
        }
    }

    fn multi_select(
        &mut self,
        message: &str,
        items: Vec<String>,
        defaults: &[usize],
    ) -> Result<Option<Vec<usize>>> {
        let selected = MultiSelect::new(message, items)
            .with_default(defaults)
            .with_formatter(&|options| format!("{} 件", options.len()))
            .with_help_message("↑↓ で移動、Space で選択を切り替え、Enter で決定、Esc で取りやめ")
            .raw_prompt();
        match selected {
            Ok(options) => Ok(Some(
                options.into_iter().map(|option| option.index).collect(),
            )),
            Err(InquireError::OperationCanceled) => Ok(None),
            Err(InquireError::OperationInterrupted) => Err(Abort::Interrupted.into()),
            Err(error) => Err(error.into()),
        }
    }

    fn read_line(&mut self, question: &str) -> Result<String> {
        let mut stderr = io::stderr();
        write!(stderr, "{question}")?;
        stderr.flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        Ok(answer)
    }
}

/// 名前が無ければ、設定のプロファイルを一覧で出して選ばせる
pub fn select_profile<'a>(
    config: &'a Config,
    name: Option<&'a str>,
) -> Result<(&'a str, &'a Profile)> {
    select_profile_with(config, name, &mut Terminal)
}

fn select_profile_with<'a>(
    config: &'a Config,
    name: Option<&'a str>,
    console: &mut impl Console,
) -> Result<(&'a str, &'a Profile)> {
    if let Some(name) = name {
        return Ok((name, config.profile(name)?));
    }
    if !console.stdin_is_terminal() {
        bail!(
            "標準入力がターミナルではないため、プロファイルを一覧から選べません。プロファイル名を指定してください"
        );
    }

    let profiles: Vec<_> = config.profiles().collect();
    match console.select("プロファイルを選んでください", choice_labels(&profiles))? {
        Some(index) => Ok(profiles[index]),
        None => Err(Abort::Cancelled("プロファイルの選択を取りやめました").into()),
    }
}

fn choice_labels(profiles: &[(&str, &Profile)]) -> Vec<String> {
    let width = profiles
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    profiles
        .iter()
        .map(|(name, profile)| format!("{name:<width$}  {} / {}", profile.cluster, profile.service))
        .collect()
}

/// `--size` の一覧から大きさを選ばせる。先頭の「タスク定義のまま」なら None
pub fn select_size(defined: DefinedSize) -> Result<Option<TaskSize>> {
    select_size_with(defined, &mut Terminal)
}

fn select_size_with(defined: DefinedSize, console: &mut impl Console) -> Result<Option<TaskSize>> {
    if !console.stdin_is_terminal() {
        bail!(
            "標準入力がターミナルではないため、大きさを一覧から選べません。--cpu / --memory で指定してください"
        );
    }
    let items = std::iter::once(format!("タスク定義のまま（{defined}）"))
        .chain(size::PRESETS.iter().map(ToString::to_string))
        .collect();
    match console.select("タスクの大きさを選んでください", items)? {
        Some(0) => Ok(None),
        Some(index) => Ok(Some(size::PRESETS[index - 1])),
        None => Err(Abort::Cancelled("大きさの選択を取りやめました").into()),
    }
}

/// タスクを起動する前に y/N を聞く。`yes` なら聞かない。size はタスク定義から変えるときだけ渡す
pub fn confirm_launch(
    name: &str,
    profile: &Profile,
    size: Option<TaskSize>,
    yes: bool,
) -> Result<()> {
    confirm_launch_with(name, profile, size, yes, &mut Terminal)
}

fn confirm_launch_with(
    name: &str,
    profile: &Profile,
    size: Option<TaskSize>,
    yes: bool,
    console: &mut impl Console,
) -> Result<()> {
    if yes {
        return Ok(());
    }
    if !console.stdin_is_terminal() {
        bail!(
            "起動前に y/N を聞きますが、標準入力がターミナルではありません。確認を省くには --yes を付けてください"
        );
    }

    let size = size.map_or(String::new(), |size| format!(" {size} で"));
    let answer = console.read_line(&format!(
        "{name}（cluster={} service={}）で使い捨てタスクを{size}起動します。よろしいですか？ [y/N] ",
        profile.cluster, profile.service
    ))?;
    if is_yes(&answer) {
        Ok(())
    } else {
        Err(Abort::Cancelled("起動を取りやめました").into())
    }
}

/// question を出して 1 行読む。読んでいる間も、呼び出し側はシグナルを待てる
///
/// tokio の spawn_blocking で読まないのは、答えを待たずに終了するとき、ランタイムの終了が stdin の読み込みを待ち続けて終われなくなるから
pub async fn read_line_concurrently(question: &str) -> Result<String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let question = question.to_owned();
    std::thread::spawn(move || {
        let _ = sender.send(Terminal.read_line(&question));
    });
    receiver
        .await
        .context("標準入力を読むスレッドが答えを返さずに終わりました")?
}

pub fn stdin_is_terminal() -> bool {
    Terminal.stdin_is_terminal()
}

pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
        [profiles.staging]
        region = "us-east-1"
        cluster = "example-staging"
        service = "worker"
        container = "app"

        [profiles.production]
        region = "us-east-1"
        cluster = "example-production"
        service = "worker"
        container = "app"
    "#;

    /// 呼ばれた内容を記録し、決めておいた答えを返す
    #[derive(Default)]
    struct FakeConsole {
        is_terminal: bool,
        selection: Option<usize>,
        answer: String,
        shown_items: Option<Vec<String>>,
        asked: Option<String>,
    }

    impl FakeConsole {
        fn terminal() -> Self {
            Self {
                is_terminal: true,
                ..Self::default()
            }
        }

        fn answering(answer: &str) -> Self {
            Self {
                answer: answer.into(),
                ..Self::terminal()
            }
        }
    }

    impl Console for FakeConsole {
        fn stdin_is_terminal(&self) -> bool {
            self.is_terminal
        }

        fn select(&mut self, _message: &str, items: Vec<String>) -> Result<Option<usize>> {
            self.shown_items = Some(items);
            Ok(self.selection)
        }

        fn multi_select(
            &mut self,
            _message: &str,
            _items: Vec<String>,
            _defaults: &[usize],
        ) -> Result<Option<Vec<usize>>> {
            unreachable!("プロファイルの選択と確認では複数選択を出さない")
        }

        fn read_line(&mut self, question: &str) -> Result<String> {
            self.asked = Some(question.into());
            Ok(self.answer.clone())
        }
    }

    fn config() -> Config {
        toml::from_str(CONFIG).unwrap()
    }

    fn abort_of(result: Result<impl fmt::Debug>) -> Abort {
        result.unwrap_err().downcast::<Abort>().unwrap()
    }

    #[test]
    fn named_profile_is_used_without_showing_the_list() {
        let config = config();
        let mut console = FakeConsole::default();

        let (name, profile) = select_profile_with(&config, Some("staging"), &mut console).unwrap();

        assert_eq!(name, "staging");
        assert_eq!(profile.cluster, "example-staging");
        assert_eq!(console.shown_items, None);
    }

    #[test]
    fn omitted_profile_is_chosen_from_the_list_with_cluster_and_service() {
        let config = config();
        let mut console = FakeConsole {
            selection: Some(0),
            ..FakeConsole::terminal()
        };

        let (name, _) = select_profile_with(&config, None, &mut console).unwrap();

        assert_eq!(name, "production");
        assert_eq!(
            console.shown_items.unwrap(),
            [
                "production  example-production / worker",
                "staging     example-staging / worker",
            ]
        );
    }

    #[test]
    fn omitted_profile_without_terminal_is_an_error() {
        let config = config();
        let mut console = FakeConsole::default();

        let message = select_profile_with(&config, None, &mut console)
            .unwrap_err()
            .to_string();

        assert!(message.contains("標準入力がターミナルではない"));
        assert_eq!(console.shown_items, None);
    }

    #[test]
    fn escaping_the_list_cancels_with_exit_code_1() {
        let config = config();

        let abort = abort_of(select_profile_with(
            &config,
            None,
            &mut FakeConsole::terminal(),
        ));

        assert_eq!(
            abort,
            Abort::Cancelled("プロファイルの選択を取りやめました")
        );
        assert_eq!(abort.exit_code(), 1);
    }

    #[test]
    fn interrupt_exits_with_130_without_message() {
        assert_eq!(Abort::Interrupted.exit_code(), 130);
        assert_eq!(Abort::Interrupted.message(), None);
    }

    #[test]
    fn every_profile_is_confirmed_before_launch() {
        let config = config();

        for (name, profile) in config.profiles() {
            let mut console = FakeConsole::answering("y\n");

            confirm_launch_with(name, profile, None, false, &mut console).unwrap();

            assert!(console.asked.is_some(), "{name}");
        }
    }

    #[test]
    fn yes_skips_confirmation_even_without_terminal() {
        let config = config();
        let mut console = FakeConsole::default();

        confirm_launch_with(
            "production",
            config.profile("production").unwrap(),
            None,
            true,
            &mut console,
        )
        .unwrap();

        assert_eq!(console.asked, None);
    }

    #[test]
    fn confirmation_without_terminal_is_an_error_suggesting_yes() {
        let config = config();

        let message = confirm_launch_with(
            "production",
            config.profile("production").unwrap(),
            None,
            false,
            &mut FakeConsole::default(),
        )
        .unwrap_err()
        .to_string();

        assert!(message.contains("標準入力がターミナルではありません"));
        assert!(message.contains("--yes"));
    }

    #[test]
    fn confirmation_names_profile_cluster_and_service() {
        let config = config();
        let mut console = FakeConsole::answering("y\n");

        confirm_launch_with(
            "production",
            config.profile("production").unwrap(),
            None,
            false,
            &mut console,
        )
        .unwrap();

        assert_eq!(
            console.asked.unwrap(),
            "production（cluster=example-production service=worker）で使い捨てタスクを起動します。よろしいですか？ [y/N] "
        );
    }

    #[test]
    fn confirmation_names_the_size_when_it_differs_from_the_task_definition() {
        let config = config();
        let mut console = FakeConsole::answering("y\n");
        let size = TaskSize {
            cpu: "2".parse().unwrap(),
            memory: "8GB".parse().unwrap(),
        };

        confirm_launch_with(
            "production",
            config.profile("production").unwrap(),
            Some(size),
            false,
            &mut console,
        )
        .unwrap();

        assert_eq!(
            console.asked.unwrap(),
            "production（cluster=example-production service=worker）で使い捨てタスクを 2 vCPU / 8 GB で起動します。よろしいですか？ [y/N] "
        );
    }

    const DEFINED: DefinedSize = DefinedSize {
        cpu: Some(crate::size::Cpu::from_units(1024)),
        memory: Some(crate::size::Memory::from_mib(2048)),
    };

    #[test]
    fn size_list_starts_with_the_task_definition_size_then_the_presets() {
        let mut console = FakeConsole {
            selection: Some(0),
            ..FakeConsole::terminal()
        };

        let chosen = select_size_with(DEFINED, &mut console).unwrap();

        assert_eq!(chosen, None);
        let items = console.shown_items.unwrap();
        assert_eq!(items[0], "タスク定義のまま（1 vCPU / 2 GB）");
        assert_eq!(&items[1..], size::PRESETS.map(|p| p.to_string()));
    }

    #[test]
    fn choosing_a_preset_returns_that_size() {
        let mut console = FakeConsole {
            selection: Some(5),
            ..FakeConsole::terminal()
        };

        assert_eq!(
            select_size_with(DEFINED, &mut console).unwrap(),
            Some(size::PRESETS[4])
        );
    }

    #[test]
    fn size_list_without_terminal_is_an_error_suggesting_cpu_and_memory() {
        let message = select_size_with(DEFINED, &mut FakeConsole::default())
            .unwrap_err()
            .to_string();

        assert!(message.contains("--cpu / --memory"), "{message}");
    }

    #[test]
    fn escaping_the_size_list_cancels_with_exit_code_1() {
        let abort = abort_of(select_size_with(DEFINED, &mut FakeConsole::terminal()));

        assert_eq!(abort, Abort::Cancelled("大きさの選択を取りやめました"));
        assert_eq!(abort.exit_code(), 1);
    }

    #[test]
    fn answer_other_than_yes_cancels_launch_with_exit_code_1() {
        let config = config();
        let production = config.profile("production").unwrap();

        for answer in ["", "\n", "n\n", "no\n", "yess\n"] {
            let mut console = FakeConsole::answering(answer);

            let abort = abort_of(confirm_launch_with(
                "production",
                production,
                None,
                false,
                &mut console,
            ));

            assert_eq!(
                abort,
                Abort::Cancelled("起動を取りやめました"),
                "{answer:?}"
            );
            assert_eq!(abort.exit_code(), 1);
        }
    }

    #[test]
    fn only_y_or_yes_ignoring_case_and_spaces_is_yes() {
        for answer in ["y\n", "Y\n", "yes\n", "YES\n", "  y  \n"] {
            assert!(is_yes(answer), "{answer:?}");
        }
        for answer in ["", "\n", "n\n", "no\n", "yess\n", "ｙ\n"] {
            assert!(!is_yes(answer), "{answer:?}");
        }
    }
}
