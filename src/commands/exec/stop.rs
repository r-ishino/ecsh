use std::fmt;
use std::time::Duration;

use anyhow::Result;
use aws_sdk_ecs::Client;

use crate::aws_error;
use crate::aws_profile::AwsProfile;
use crate::ecs;
use crate::report::report;
use crate::signals::{Interruption, Signal, Signals, Stage};
use crate::ui::{self, Guidance, Style};

/// 起動したタスクを止め、タスクを使っていた間の結果 `used` と StopTask の結果をまとめて返す
pub async fn stop_after(
    client: &Client,
    cluster: &str,
    task_arn: &str,
    used: Result<()>,
    in_session: Option<Duration>,
    signals: &mut Signals,
    aws_profile: &AwsProfile,
) -> Result<()> {
    let style = Style::current();
    let stage = match used
        .as_ref()
        .err()
        .and_then(|e| e.downcast_ref::<Interruption>())
    {
        Some(interruption) => {
            report!(
                "{}",
                style.warning(format!(
                    "{} を受けたので、タスクを止めてから終了します（もう一度 Ctrl-C を押すと待たずに終了します）",
                    interruption.signal
                ))
            );
            Stage::StoppingOnSignal
        }
        None => Stage::Stopping,
    };
    let stopped = signals
        .watch(stage, ecs::stop_task(client, cluster, task_arn))
        .await
        .map_err(|interruption| StopAbandoned {
            task_arn: task_arn.to_owned(),
            signal: interruption.signal,
        })?
        .map_err(|error| aws_error::explain(error, aws_profile));
    if stopped.is_ok() {
        report!("{}", style.success(stopped_message(task_arn, in_session)));
    }
    conclude(used, stopped, task_arn)
}

/// StopTask の応答を待たずに終了した。タスクは残っているかもしれない
#[derive(Debug)]
pub struct StopAbandoned {
    task_arn: String,
    pub signal: Signal,
}

impl fmt::Display for StopAbandoned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "タスクを止めるのを待たずに終了しました: {}。`ecsh gc` で止められます",
            self.task_arn
        )
    }
}

impl std::error::Error for StopAbandoned {}

/// `タスクを止めました  01234567（入っていた時間 12 分）`。セッションに入る前に止めたときは時間を添えない
fn stopped_message(task_arn: &str, in_session: Option<Duration>) -> String {
    let id = ui::short_task_id(task_arn);
    match in_session {
        Some(elapsed) => format!(
            "タスクを止めました  {id}（入っていた時間 {}）",
            ui::duration(elapsed)
        ),
        None => format!("タスクを止めました  {id}"),
    }
}

fn conclude(used: Result<()>, stopped: Result<()>, task_arn: &str) -> Result<()> {
    let Err(stop_error) = stopped else {
        return used;
    };
    let stop_error = match used {
        Ok(()) => stop_error,
        Err(error) => stop_error.context(format!("止める前に起きたエラー: {error:#}")),
    };
    Err(Guidance::new(
        format!("タスクを止められませんでした: {task_arn}"),
        "`ecsh gc` で止められます",
    )
    .wrap(stop_error))
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    const TASK_ARN: &str = "arn:aws:ecs:us-east-1:123456789012:task/c/0123456789abcdef";

    fn shown(error: &anyhow::Error) -> Vec<String> {
        Style::PLAIN.error_lines(error)
    }

    #[test]
    fn stopping_after_successful_use_succeeds() {
        assert!(conclude(Ok(()), Ok(()), TASK_ARN).is_ok());
    }

    #[test]
    fn error_while_using_the_task_is_returned_as_is_once_the_task_is_stopped() {
        let error = conclude(Err(anyhow!("agent timed out")), Ok(()), TASK_ARN).unwrap_err();

        assert_eq!(format!("{error:#}"), "agent timed out");
    }

    #[test]
    fn stop_failure_names_the_full_task_arn_left_behind_and_points_to_gc() {
        let error = conclude(Ok(()), Err(anyhow!("expired token")), TASK_ARN).unwrap_err();

        assert_eq!(
            shown(&error),
            [
                format!("✗ タスクを止められませんでした: {TASK_ARN}"),
                "  `ecsh gc` で止められます".into(),
                "  expired token".into(),
            ]
        );
    }

    #[test]
    fn stop_failure_after_an_error_while_using_the_task_reports_both() {
        let error = conclude(
            Err(anyhow!("agent timed out")),
            Err(anyhow!("expired token")),
            TASK_ARN,
        )
        .unwrap_err();

        assert_eq!(
            shown(&error),
            [
                format!("✗ タスクを止められませんでした: {TASK_ARN}"),
                "  `ecsh gc` で止められます".into(),
                "  止める前に起きたエラー: agent timed out".into(),
                "  expired token".into(),
            ]
        );
    }

    #[test]
    fn stop_failure_from_an_expired_sso_session_shows_both_gc_and_sso_login() {
        let expired = Guidance::new(
            "AWS の SSO セッションが切れています",
            "`aws sso login` を実行してから、もう一度実行してください",
        )
        .wrap(anyhow!("StopTask に失敗しました（cluster=c）"));

        let error = conclude(Ok(()), Err(expired), TASK_ARN).unwrap_err();

        assert_eq!(
            shown(&error),
            [
                format!("✗ タスクを止められませんでした: {TASK_ARN}"),
                "  `ecsh gc` で止められます".into(),
                "  `aws sso login` を実行してから、もう一度実行してください".into(),
                "  AWS の SSO セッションが切れています".into(),
                "  StopTask に失敗しました（cluster=c）".into(),
            ]
        );
    }

    #[test]
    fn stopped_message_shows_short_task_id_and_time_spent_in_the_session() {
        assert_eq!(
            stopped_message(TASK_ARN, Some(Duration::from_secs(720))),
            "タスクを止めました  01234567（入っていた時間 12 分）"
        );
    }

    #[test]
    fn stopped_message_before_entering_the_session_has_no_time() {
        assert_eq!(
            stopped_message(TASK_ARN, None),
            "タスクを止めました  01234567"
        );
    }
}
