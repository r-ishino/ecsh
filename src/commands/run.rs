use std::borrow::Cow;
use std::time::Instant;

use anyhow::Result;
use aws_sdk_ecs::Client;

use super::exec::StopAbandoned;
use super::launch::{self, Prepared};
use crate::aws_error;
use crate::aws_profile::AwsProfile;
use crate::config::Profile;
use crate::ecs::{self, LaunchedTask, Workload};
use crate::logs::{self, LogDestination, LogTail};
use crate::prompt;
use crate::report::report;
use crate::signals::{Action, Interruption, Signals, Stage};
use crate::ui::{self, Guidance, Style};

mod follow;
mod outcome;

use follow::Follower;

const STOP_QUESTION: &str =
    "タスクを止めますか？ [y/N]（N なら手元だけ抜け、タスクは最後まで動きます） ";

/// `ecsh run` に `--` の後ろのコマンドが無い。何も起動せず、入るなら exec を案内する
pub fn missing_command(profile: Option<&str>) -> anyhow::Error {
    let profile = profile.unwrap_or("<プロファイル>");
    anyhow::Error::new(Guidance::new(
        format!(
            "流すコマンドがありません。`ecsh run {profile} -- <コマンド>` の形で渡してください"
        ),
        format!("対話で入るなら `ecsh exec {profile}`"),
    ))
}

/// command をタスクのコマンドにして起動する。`detach` でなければ止まるまで出力を流し、ecsh の終了コードを返す
pub async fn run(
    name: &str,
    profile: &Profile,
    command: &[String],
    yes: bool,
    detach: bool,
) -> Result<u8> {
    let style = Style::current();
    let Prepared {
        aws_profile,
        client,
        snapshot,
        started_by,
    } = launch::prepare(
        name,
        profile,
        yes,
        Some(format!("コマンド  {}", style.bold(command_line(command)))),
    )
    .await?;
    let explain = |error| aws_error::explain(error, &aws_profile);

    // RunTask の後で登録すると、RunTask の最中の Ctrl-C で、動き出したタスクの ID を知らせないまま終了する。ここで受けたシグナルは待ち始めてから拾う
    let mut signals = Signals::listen()?;
    let task = ecs::run_task(
        &client,
        &profile.cluster,
        &profile.container,
        &snapshot,
        &started_by,
        Workload::Command(command),
    )
    .await
    .map_err(explain)?;
    let launched_at = Instant::now();
    report!(
        "{}",
        style.success(format!(
            "タスクを起動しました  {}（{}）",
            ui::short_task_id(&task.task_arn),
            ui::task_definition_name(&task.task_definition_arn)
        ))
    );
    report!(
        "{}",
        style.note(format!("上限時間はありません · startedBy {started_by}"))
    );

    let destination = log_destination(&client, profile, &task, &aws_profile).await;
    match &destination {
        LogDestination::Awslogs(stream) => report!("{}", style.note(format!("ログ  {stream}"))),
        LogDestination::Unreadable(reason) => report!(
            "{}",
            style.warning(format!(
                "{reason}ので、コマンドの出力は流さず、終わるのを待ちます"
            ))
        ),
    }
    if detach {
        report!(
            "{}",
            style.note("待たずに抜けます。タスクは最後まで動きます")
        );
        for line in outcome::whereabouts_lines(style, name, &task.task_arn, &destination) {
            report!("{line}");
        }
        return Ok(0);
    }
    report!(
        "{}",
        style.note("Ctrl-C で、タスクを止めるか手元だけ抜けるかを選べます")
    );
    report!();

    let logs = match &destination {
        LogDestination::Awslogs(stream) => Some(LogTail::new(
            logs::client(&stream.region, &aws_profile).await,
            stream.clone(),
        )),
        LogDestination::Unreadable(_) => None,
    };
    let mut follower = Follower::new(
        &client,
        &profile.cluster,
        &task.task_arn,
        &profile.container,
        logs,
    );
    let stopped = match signals
        .watch(Stage::Following, follower.until_stopped())
        .await
    {
        Ok(stopped) => stopped.map_err(|error| lost_sight(explain(error), &task.task_arn))?,
        Err(interruption) => {
            let interrupted = Interrupted {
                client: &client,
                cluster: &profile.cluster,
                task_arn: &task.task_arn,
                profile_name: name,
                destination: &destination,
                aws_profile: &aws_profile,
            };
            return interrupted
                .handle(interruption, &mut signals, &mut follower)
                .await;
        }
    };
    for line in outcome::finished_lines(
        style,
        &stopped,
        launched_at.elapsed(),
        follower.missing_stream(),
    ) {
        report!("{line}");
    }
    Ok(outcome::exit_code(&stopped))
}

/// 起動したタスクのコンテナのログ設定から、出力の送り先を求める。求められなくても run は続け、終わるのを待つ
async fn log_destination(
    client: &Client,
    profile: &Profile,
    task: &LaunchedTask,
    aws_profile: &AwsProfile,
) -> LogDestination {
    match ecs::container_definition(client, &task.task_definition_arn, &profile.container).await {
        Ok(definition) => logs::destination(
            definition.log_configuration(),
            &profile.container,
            ui::task_id(&task.task_arn),
            &profile.region,
        ),
        Err(error) => {
            let error = aws_error::explain(error, aws_profile);
            let lines = Style::current().warning_lines("ログの設定を読めませんでした", &error);
            for line in lines {
                report!("{line}");
            }
            LogDestination::Unreadable("ログの送り先が分からない".into())
        }
    }
}

/// 見届けている途中で AWS を呼べなくなった。タスクは動き続けている
fn lost_sight(error: anyhow::Error, task_arn: &str) -> anyhow::Error {
    Guidance::new(
        format!("タスクを見届けられませんでした: {task_arn}"),
        "タスクは動き続けています。止めるなら `ecsh gc` の一覧で選んでください",
    )
    .wrap(error)
}

/// 待っている間にシグナルを受けたときの後始末に要るもの
struct Interrupted<'a> {
    client: &'a Client,
    cluster: &'a str,
    task_arn: &'a str,
    profile_name: &'a str,
    destination: &'a LogDestination,
    aws_profile: &'a AwsProfile,
}

impl Interrupted<'_> {
    async fn handle(
        &self,
        interruption: Interruption,
        signals: &mut Signals,
        follower: &mut Follower<'_>,
    ) -> Result<u8> {
        let style = Style::current();
        report!();
        if !asks_to_stop(interruption, prompt::stdin_is_terminal()) {
            return Ok(self.leave(interruption));
        }
        let answer = match signals
            .watch(
                Stage::AskingToStop,
                prompt::read_line_concurrently(STOP_QUESTION),
            )
            .await
        {
            Ok(answer) => answer?,
            Err(second) => {
                report!();
                return Ok(self.leave(second));
            }
        };
        if !prompt::is_yes(&answer) {
            return Ok(self.leave(interruption));
        }

        report!(
            "{}",
            style.warning("タスクを止めます（もう一度 Ctrl-C を押すと待たずに終了します）")
        );
        signals
            .watch(
                Stage::StoppingOnSignal,
                ecs::stop_task(self.client, self.cluster, self.task_arn, ecs::STOP_REASON),
            )
            .await
            .map_err(|second| StopAbandoned::new(self.task_arn, second.signal))?
            .map_err(|error| {
                Guidance::new(
                    format!("タスクを止められませんでした: {}", self.task_arn),
                    "`ecsh gc` で止められます",
                )
                .wrap(aws_error::explain(error, self.aws_profile))
            })?;
        report!(
            "{}",
            style.note("止まるのを待っています（Ctrl-C で待たずに終了します）")
        );
        match signals
            .watch(Stage::AwaitingStopped, follower.until_stopped())
            .await
        {
            Ok(stopped) => {
                let stopped = stopped
                    .map_err(|error| aws_error::explain(error, self.aws_profile))
                    .map_err(|error| lost_sight(error, self.task_arn))?;
                report!(
                    "{}",
                    outcome::stopped_on_request_line(style, self.task_arn, &stopped)
                );
            }
            Err(second) => {
                report!(
                    "{}",
                    style.warning(format!(
                        "止まるのを待たずに終了しました。止める指示は送ってあります: {}",
                        ui::short_task_id(self.task_arn)
                    ))
                );
                return Ok(second.signal.exit_code());
            }
        }
        Ok(interruption.signal.exit_code())
    }

    /// 止めずに手元だけ抜ける。終了コードはシグナルの慣習どおり
    fn leave(&self, interruption: Interruption) -> u8 {
        let style = Style::current();
        report!(
            "{}",
            style.warning(format!(
                "{} を受けて手元だけ抜けました。タスクは最後まで動きます",
                interruption.signal
            ))
        );
        for line in
            outcome::whereabouts_lines(style, self.profile_name, self.task_arn, self.destination)
        {
            report!("{line}");
        }
        interruption.signal.exit_code()
    }
}

/// 止めるかを聞くのは、Ctrl-C を受け、答えを読めるときだけ。それ以外は止めずに抜ける
fn asks_to_stop(interruption: Interruption, stdin_is_terminal: bool) -> bool {
    interruption.action == Action::AskToStop && stdin_is_terminal
}

/// 確かめる用に、コマンドをシェルに貼れる形で 1 行にする
fn command_line(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote(arg: &str) -> Cow<'_, str> {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if plain {
        Cow::Borrowed(arg)
    } else {
        Cow::Owned(format!("'{}'", arg.replace('\'', r"'\''")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::{self, Signal};

    fn shown(error: &anyhow::Error) -> Vec<String> {
        Style::PLAIN.error_lines(error)
    }

    #[test]
    fn missing_command_launches_nothing_and_points_to_exec_with_the_profile() {
        assert_eq!(
            shown(&missing_command(Some("staging"))),
            [
                "✗ 流すコマンドがありません。`ecsh run staging -- <コマンド>` の形で渡してください",
                "  対話で入るなら `ecsh exec staging`",
            ]
        );
    }

    #[test]
    fn missing_command_without_a_profile_uses_a_placeholder() {
        assert_eq!(
            shown(&missing_command(None))[1],
            "  対話で入るなら `ecsh exec <プロファイル>`"
        );
    }

    #[test]
    fn only_ctrl_c_with_a_terminal_on_stdin_asks_whether_to_stop_the_task() {
        let received = |signal| Interruption {
            signal,
            action: signals::action(Stage::Following, signal),
        };

        assert!(asks_to_stop(received(Signal::Interrupt), true));
        assert!(!asks_to_stop(received(Signal::Interrupt), false));
        for signal in [Signal::Hangup, Signal::Terminate] {
            assert!(!asks_to_stop(received(signal), true), "{signal}");
        }
    }

    #[test]
    fn command_line_quotes_only_arguments_the_shell_would_split_or_expand() {
        let command = [
            "bundle",
            "exec",
            "rake",
            "users:import[2026-09-01,dry]",
            "LIMIT=10",
            "it's",
            "",
        ]
        .map(String::from);

        assert_eq!(
            command_line(&command),
            r#"bundle exec rake 'users:import[2026-09-01,dry]' LIMIT=10 'it'\''s' ''"#
        );
    }
}
