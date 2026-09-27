use std::fmt;

use anyhow::Result;
use aws_sdk_ecs::Client;

use crate::ecs;
use crate::report::report;
use crate::signals::{Interruption, Signal, Signals, Stage};

/// 起動したタスクを止め、タスクを使っていた間の結果 `used` と StopTask の結果をまとめて返す
pub async fn stop_after(
    client: &Client,
    cluster: &str,
    task_arn: &str,
    used: Result<()>,
    signals: &mut Signals,
) -> Result<()> {
    let stage = match used
        .as_ref()
        .err()
        .and_then(|e| e.downcast_ref::<Interruption>())
    {
        Some(interruption) => {
            report!(
                "{} を受けたので、タスクを止めてから終了します（もう一度 Ctrl-C を押すと待たずに終了します）",
                interruption.signal
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
        })?;
    if stopped.is_ok() {
        report!("タスクを止めました: {task_arn}");
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

fn conclude(used: Result<()>, stopped: Result<()>, task_arn: &str) -> Result<()> {
    let Err(stop_error) = stopped else {
        return used;
    };
    let left_behind = format!("タスクを止められませんでした: {task_arn}。`ecsh gc` で止められます");
    let message = match used {
        Ok(()) => left_behind,
        Err(error) => format!("{left_behind}\n止める前に起きたエラー: {error:#}"),
    };
    Err(stop_error.context(message))
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    const TASK_ARN: &str = "arn:aws:ecs:us-east-1:123456789012:task/c/abc";

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
    fn stop_failure_names_the_task_left_behind_and_points_to_gc() {
        let error = conclude(Ok(()), Err(anyhow!("expired token")), TASK_ARN).unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains(TASK_ARN));
        assert!(message.contains("`ecsh gc` で止められます"));
        assert!(message.contains("expired token"));
    }

    #[test]
    fn stop_failure_after_an_error_while_using_the_task_reports_both() {
        let error = conclude(
            Err(anyhow!("agent timed out")),
            Err(anyhow!("expired token")),
            TASK_ARN,
        )
        .unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains(TASK_ARN));
        assert!(message.contains("`ecsh gc` で止められます"));
        assert!(message.contains("agent timed out"));
        assert!(message.contains("expired token"));
    }
}
