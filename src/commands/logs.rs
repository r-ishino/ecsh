use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use aws_sdk_ecs::Client;
use aws_sdk_ecs::operation::describe_tasks::DescribeTasksOutput;
use chrono::{DateTime, Local, Utc};

use super::run::follow::{self, Follower, Progress, Stopped};
use super::run::{self, command_line, outcome};
use crate::aws_error;
use crate::aws_profile::AwsProfile;
use crate::config::Config;
use crate::console;
use crate::ecs;
use crate::history::{self, Entry, History};
use crate::logs::{self, LogTail};
use crate::prompt::{self, Terminal};
use crate::report::report;
use crate::signals::{Signals, Stage};
use crate::ui::{self, Style};

mod choice;

/// 手元の run の履歴から 1 件選び、出力を見る。ecsh の終了コードを返す
///
/// `last` なら直近の 1 件を選ばせずに開く。そのときプロファイルを省略したら、プロファイルを問わず直近
///
/// `in_browser` なら、出力を流す代わりに CloudWatch Logs のページをブラウザで開く
pub async fn logs(
    config: &Config,
    profile: Option<&str>,
    last: bool,
    in_browser: bool,
) -> Result<u8> {
    let profile = match (profile, last) {
        (None, true) => None,
        (profile, _) => Some(prompt::select_profile(config, profile)?.0),
    };
    let entries = History::open()?.load()?;
    let candidates = history::newest_first(&entries, profile);
    let Some(entry) = choice::choose(&candidates, last, &mut Terminal, &Local)? else {
        match profile {
            Some(name) => report!("{name} で run した履歴はありません"),
            None => report!("run した履歴はありません"),
        }
        return Ok(0);
    };
    if in_browser {
        console::open_in_browser(&log_page_url(entry)?)?;
        return Ok(0);
    }
    open(entry).await
}

fn log_page_url(entry: &Entry) -> Result<String> {
    match &entry.log {
        Some(stream) => Ok(console::log_stream_url(stream)),
        None => bail!(
            "この run の出力は CloudWatch Logs から読めない設定だったので、ログのページを開けません"
        ),
    }
}

async fn open(entry: &Entry) -> Result<u8> {
    let style = Style::current();
    let aws_profile = AwsProfile::resolve(entry.aws_profile.as_deref())?;
    let explain = |error| aws_error::explain(error, &aws_profile);
    report!();
    report!(
        "  {}  →  {} · {} · {} に起動",
        style.bold(&entry.profile),
        entry.cluster,
        ui::short_task_id(&entry.task_arn),
        choice::launched_at(entry, &Local)
    );
    report!("  コマンド  {}", style.bold(command_line(&entry.command)));
    if let Some(stream) = &entry.log {
        report!("{}", style.note(format!("ログ  {stream}")));
    }
    report!();

    let finish = match &entry.finish {
        Some(finish) => Some((Stopped::from(finish), finish.took())),
        None => {
            let client = ecs::client(&entry.region, &aws_profile).await;
            match task_state(&client, entry).await.map_err(explain)? {
                TaskState::Gone => None,
                TaskState::Stopped(stopped, took) => {
                    run::record_finish(&entry.task_arn, &stopped, took);
                    Some((stopped, took))
                }
                TaskState::Running { started } => {
                    return follow(entry, &client, &aws_profile, started).await;
                }
            }
        }
    };

    print_all(entry, &aws_profile).await?;
    match finish {
        Some((stopped, took)) => {
            for line in outcome::finished_lines(style, &stopped, took, None) {
                report!("{line}");
            }
        }
        None => report!(
            "{}",
            style.warning("終了コードは分かりません（ECS にタスクの記録が残っていません）")
        ),
    }
    Ok(0)
}

/// 動いているタスクを、run で待つときと同じく止まるまで見届ける。どのシグナルでも止めずに抜ける
async fn follow(
    entry: &Entry,
    client: &Client,
    aws_profile: &AwsProfile,
    started: bool,
) -> Result<u8> {
    let style = Style::current();
    report!(
        "{}",
        style.note("コマンドはまだ動いています。Ctrl-C で抜けてもタスクは止めません")
    );
    report!();
    let mut signals = Signals::listen()?;
    let logs = match &entry.log {
        Some(stream) => Some(LogTail::new(
            logs::client(&stream.region, aws_profile).await,
            stream.clone(),
        )),
        None => None,
    };
    let mut follower = Follower::new(
        client,
        &entry.cluster,
        &entry.task_arn,
        &entry.container,
        logs,
    );
    if started {
        follower = follower.already_started();
    }
    let stopped = match signals
        .watch(Stage::Following, follower.until_stopped())
        .await
    {
        Ok(stopped) => stopped.map_err(|error| aws_error::explain(error, aws_profile))?,
        Err(interruption) => {
            report!();
            report!(
                "{}",
                style.warning(format!(
                    "{} を受けて抜けました。タスクは動き続けています",
                    interruption.signal
                ))
            );
            return Ok(interruption.signal.exit_code());
        }
    };
    let took = since(entry.launched_at, Utc::now());
    run::record_finish(&entry.task_arn, &stopped, took);
    for line in outcome::finished_lines(style, &stopped, took, follower.missing_stream()) {
        report!("{line}");
    }
    Ok(0)
}

/// 終わったタスクの出力を、全部 stdout に出す
async fn print_all(entry: &Entry, aws_profile: &AwsProfile) -> Result<()> {
    let style = Style::current();
    let Some(stream) = &entry.log else {
        report!(
            "{}",
            style.note("この run の出力は CloudWatch Logs から読めない設定でした")
        );
        return Ok(());
    };
    let mut tail = LogTail::new(
        logs::client(&stream.region, aws_profile).await,
        stream.clone(),
    );
    let messages = tail
        .read_new()
        .await
        .map_err(|error| aws_error::explain(error, aws_profile))?;
    if !tail.found() {
        report!(
            "{}",
            style.warning("ログが残っていません（保持期間を過ぎた可能性）")
        );
        return Ok(());
    }
    print_lines(&messages)
}

/// `ecsh logs --last | head` のように読み手が先に閉じたら、残りを捨てて正常に終える
fn print_lines(lines: &[String]) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let written = lines
        .iter()
        .try_for_each(|line| writeln!(stdout, "{line}"))
        .and_then(|()| stdout.flush());
    match written {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        written => written.context("標準出力に書けません"),
    }
}

/// 履歴に結果が無い run のタスクの、今の様子
#[derive(Debug, PartialEq)]
enum TaskState {
    /// 止まってしばらく経ち、DescribeTasks で見えなくなった
    Gone,
    /// `started` はコマンドが動き始めているか
    Running { started: bool },
    /// took は起動してから止まるまで
    Stopped(Stopped, Duration),
}

async fn task_state(client: &Client, entry: &Entry) -> Result<TaskState> {
    // ecs::describe_task は見えなくなったタスクもエラーにするので、MISSING を見分けるためにここで呼ぶ
    let output = client
        .describe_tasks()
        .cluster(&entry.cluster)
        .tasks(&entry.task_arn)
        .send()
        .await
        .with_context(|| format!("DescribeTasks に失敗しました（task={}）", entry.task_arn))?;
    task_state_from(&output, entry)
}

fn task_state_from(output: &DescribeTasksOutput, entry: &Entry) -> Result<TaskState> {
    if let Some(failure) = output.failures().first() {
        if failure.reason() == Some("MISSING") {
            return Ok(TaskState::Gone);
        }
        bail!(
            "タスクの状態を取得できません（reason={} detail={}）",
            failure.reason().unwrap_or("-"),
            failure.detail().unwrap_or("-")
        );
    }
    let task = output
        .tasks()
        .first()
        .context("DescribeTasks の応答にタスクがありません")?;
    Ok(match follow::progress_from(task, &entry.container) {
        Progress::Starting(_) => TaskState::Running { started: false },
        Progress::Running(_) => TaskState::Running { started: true },
        Progress::Stopped(stopped) => {
            let stopped_at = task
                .stopped_at()
                .and_then(|time| DateTime::<Utc>::from_timestamp(time.secs(), 0))
                .context("止まったタスクに停止時刻がありません")?;
            TaskState::Stopped(stopped, since(entry.launched_at, stopped_at))
        }
    })
}

/// 時計がずれて逆転していたら 0
fn since(from: DateTime<Utc>, to: DateTime<Utc>) -> Duration {
    (to - from).to_std().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use aws_sdk_ecs::primitives::DateTime as AwsDateTime;
    use aws_sdk_ecs::types::{Container, Failure, Task};

    use super::*;
    use crate::history::tests::entry;

    fn output_with(task: Task) -> DescribeTasksOutput {
        DescribeTasksOutput::builder().tasks(task).build()
    }

    fn failure(reason: &str) -> DescribeTasksOutput {
        DescribeTasksOutput::builder()
            .failures(Failure::builder().reason(reason).build())
            .build()
    }

    #[test]
    fn task_no_longer_known_to_ecs_is_gone() {
        assert_eq!(
            task_state_from(&failure("MISSING"), &entry("staging", "aaa", 0)).unwrap(),
            TaskState::Gone
        );
    }

    #[test]
    fn other_describe_failures_are_errors() {
        let message = task_state_from(&failure("ACCESS_DENIED"), &entry("staging", "aaa", 0))
            .unwrap_err()
            .to_string();

        assert!(message.contains("ACCESS_DENIED"), "{message}");
    }

    #[test]
    fn running_task_is_followed_telling_whether_the_command_has_started() {
        let pending = Task::builder().last_status("PENDING").build();
        let running = Task::builder()
            .last_status("RUNNING")
            .started_at(AwsDateTime::from_secs(0))
            .build();
        let only = entry("staging", "aaa", 0);

        assert_eq!(
            task_state_from(&output_with(pending), &only).unwrap(),
            TaskState::Running { started: false }
        );
        assert_eq!(
            task_state_from(&output_with(running), &only).unwrap(),
            TaskState::Running { started: true }
        );
    }

    #[test]
    fn stopped_task_yields_the_exit_code_and_the_time_from_launch_to_stop() {
        let launched = entry("staging", "aaa", 0);
        let stopped_at = launched.launched_at.timestamp() + 125;
        let task = Task::builder()
            .last_status("STOPPED")
            .stopped_at(AwsDateTime::from_secs(stopped_at))
            .containers(Container::builder().name("app").exit_code(3).build())
            .build();

        let TaskState::Stopped(stopped, took) =
            task_state_from(&output_with(task), &launched).unwrap()
        else {
            panic!("止まったと読めない");
        };

        assert_eq!(stopped.exit_code, Some(3));
        assert_eq!(took, Duration::from_secs(125));
    }

    #[test]
    fn log_page_is_the_console_page_of_the_recorded_log_stream() {
        let recorded = entry("staging", "aaa", 0);

        assert_eq!(
            log_page_url(&recorded).unwrap(),
            console::log_stream_url(recorded.log.as_ref().unwrap())
        );
    }

    #[test]
    fn log_page_cannot_be_opened_for_a_run_without_readable_logs() {
        let mut unreadable = entry("staging", "aaa", 0);
        unreadable.log = None;

        assert!(log_page_url(&unreadable).is_err());
    }

    #[test]
    fn time_taken_is_zero_rather_than_negative_when_clocks_disagree() {
        let later = entry("staging", "aaa", 5).launched_at;
        let earlier = entry("staging", "aaa", 0).launched_at;

        assert_eq!(since(later, earlier), Duration::ZERO);
    }
}
