use std::time::Duration;

use super::follow::Stopped;
use crate::logs::{LogDestination, LogStream};
use crate::ui::{self, Style};

/// コマンドの終了コードで ecsh も終わる。終了コードが無ければ 1
pub fn exit_code(stopped: &Stopped) -> u8 {
    stopped
        .exit_code
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1)
}

/// コマンドが終わったときの結果の行
pub fn finished_lines(
    style: Style,
    stopped: &Stopped,
    took: Duration,
    missing_stream: Option<&LogStream>,
) -> Vec<String> {
    let took = ui::duration(took);
    let mut lines = Vec::new();
    if let (Some(stream), Some(_)) = (missing_stream, stopped.exit_code) {
        lines.push(style.warning(format!(
            "ログストリーム {stream} が見つからず、出力を流せませんでした"
        )));
    }
    lines.push(match stopped.exit_code {
        Some(0) => style.success(format!(
            "コマンドが終わりました（終了コード 0 · 所要時間 {took}）"
        )),
        Some(code) => style.failure(format!(
            "コマンドが終了コード {code} で終わりました（所要時間 {took}）"
        )),
        None => style.failure(format!(
            "コマンドの終了コードがありません（所要時間 {took}）"
        )),
    });
    if stopped.exit_code.is_none() {
        let reason = stopped.stopped_reason.as_deref().unwrap_or("-");
        lines.push(style.note(format!("停止理由  {reason}")));
    }
    if let Some(reason) = &stopped.container_reason {
        lines.push(style.note(format!("コンテナ  {reason}")));
    }
    lines
}

/// Ctrl-C → y で止め、止まったのを見届けたときの行
pub fn stopped_on_request_line(style: Style, task_arn: &str, stopped: &Stopped) -> String {
    let exit_code = stopped
        .exit_code
        .map_or("なし".to_owned(), |code| code.to_string());
    style.success(format!(
        "タスクを止めました  {}（終了コード {exit_code}）",
        ui::short_task_id(task_arn)
    ))
}

/// 手元が抜けた後に、タスクをどこで追えるか。`-d` と、止めずに抜けたときに出す
pub fn whereabouts_lines(
    style: Style,
    profile_name: &str,
    task_arn: &str,
    destination: &LogDestination,
) -> Vec<String> {
    let output = match destination {
        LogDestination::Awslogs(stream) => {
            format!("出力は AWS コンソールの CloudWatch Logs で見られます  {stream}")
        }
        LogDestination::Unreadable(_) => {
            "出力は AWS コンソールの ECS のタスクの詳細から確かめてください".to_owned()
        }
    };
    vec![
        style.note(format!("タスク ID  {}", ui::task_id(task_arn))),
        style.note(output),
        style.note(format!(
            "止めるなら `ecsh gc {profile_name}` の一覧で選んでください"
        )),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASK_ARN: &str = "arn:aws:ecs:us-east-1:123456789012:task/c/0123456789abcdef";
    const PLAIN: Style = Style::PLAIN;

    fn exited(code: Option<i32>) -> Stopped {
        Stopped {
            exit_code: code,
            stopped_reason: Some("Essential container in task exited".into()),
            container_reason: None,
        }
    }

    fn stream() -> LogStream {
        LogStream {
            region: "us-east-1".into(),
            group: "/ecs/worker".into(),
            stream: "ecs/app/0123456789abcdef".into(),
        }
    }

    #[test]
    fn ecsh_exits_with_the_exit_code_of_the_command() {
        for code in [0, 1, 3, 137, 255] {
            assert_eq!(exit_code(&exited(Some(code))), code as u8);
        }
    }

    #[test]
    fn ecsh_exits_with_1_when_the_command_has_no_exit_code() {
        assert_eq!(exit_code(&exited(None)), 1);
    }

    #[test]
    fn exit_code_outside_0_to_255_is_1_so_it_is_never_mistaken_for_success() {
        for code in [-1, 256, 512] {
            assert_eq!(exit_code(&exited(Some(code))), 1, "{code}");
        }
    }

    #[test]
    fn success_shows_exit_code_and_time_taken() {
        assert_eq!(
            finished_lines(PLAIN, &exited(Some(0)), Duration::from_secs(125), None),
            ["✓ コマンドが終わりました（終了コード 0 · 所要時間 2 分 5 秒）"]
        );
    }

    #[test]
    fn failure_shows_exit_code_and_the_container_reason() {
        let stopped = Stopped {
            container_reason: Some("OutOfMemoryError".into()),
            ..exited(Some(137))
        };

        assert_eq!(
            finished_lines(PLAIN, &stopped, Duration::from_secs(45), None),
            [
                "✗ コマンドが終了コード 137 で終わりました（所要時間 45 秒）",
                "  コンテナ  OutOfMemoryError",
            ]
        );
    }

    #[test]
    fn missing_exit_code_shows_why_the_task_stopped() {
        let stopped = Stopped {
            exit_code: None,
            stopped_reason: Some("CannotPullContainerError: image not found".into()),
            container_reason: None,
        };

        assert_eq!(
            finished_lines(PLAIN, &stopped, Duration::from_secs(30), Some(&stream())),
            [
                "✗ コマンドの終了コードがありません（所要時間 30 秒）",
                "  停止理由  CannotPullContainerError: image not found",
            ]
        );
    }

    #[test]
    fn log_stream_never_found_for_a_command_that_ran_is_warned_about() {
        assert_eq!(
            finished_lines(
                PLAIN,
                &exited(Some(0)),
                Duration::from_secs(30),
                Some(&stream())
            )[0],
            "! ログストリーム /ecs/worker / ecs/app/0123456789abcdef が見つからず、出力を流せませんでした"
        );
    }

    #[test]
    fn stopping_on_request_shows_the_short_task_id_and_exit_code() {
        assert_eq!(
            stopped_on_request_line(PLAIN, TASK_ARN, &exited(Some(143))),
            "✓ タスクを止めました  01234567（終了コード 143）"
        );
        assert_eq!(
            stopped_on_request_line(PLAIN, TASK_ARN, &exited(None)),
            "✓ タスクを止めました  01234567（終了コード なし）"
        );
    }

    #[test]
    fn whereabouts_show_the_full_task_id_where_to_read_the_output_and_how_to_stop_it() {
        assert_eq!(
            whereabouts_lines(
                PLAIN,
                "staging",
                TASK_ARN,
                &LogDestination::Awslogs(stream())
            ),
            [
                "  タスク ID  0123456789abcdef",
                "  出力は AWS コンソールの CloudWatch Logs で見られます  /ecs/worker / ecs/app/0123456789abcdef",
                "  止めるなら `ecsh gc staging` の一覧で選んでください",
            ]
        );
    }

    #[test]
    fn whereabouts_without_awslogs_point_to_the_task_details() {
        let lines = whereabouts_lines(
            PLAIN,
            "staging",
            TASK_ARN,
            &LogDestination::Unreadable("コンテナにログ設定が無い".into()),
        );

        assert_eq!(
            lines[1],
            "  出力は AWS コンソールの ECS のタスクの詳細から確かめてください"
        );
    }
}
