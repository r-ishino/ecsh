//! AWS コンソールのページの URL を組み立て、ブラウザで開く

use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::logs::LogStream;
use crate::report::report;
use crate::ui::{self, Style};

/// ECS コンソールの、タスクの詳細のページ
pub fn task_url(region: &str, cluster: &str, task_arn: &str) -> String {
    format!(
        "https://{region}.console.aws.amazon.com/ecs/v2/clusters/{}/tasks/{}/configuration?region={region}",
        encode_uri_component(cluster),
        ui::task_id(task_arn)
    )
}

/// CloudWatch Logs コンソールの、ログストリームのイベントのページ
pub fn log_stream_url(stream: &LogStream) -> String {
    format!(
        "https://{region}.console.aws.amazon.com/cloudwatch/home?region={region}#logsV2:log-groups/log-group/{}/log-events/{}",
        encode_log_fragment(&stream.group),
        encode_log_fragment(&stream.stream),
        region = stream.region
    )
}

/// URL を stderr に出し、macOS の `open` でブラウザに渡す
pub fn open_in_browser(url: &str) -> Result<()> {
    report!(
        "{}",
        Style::current().note(format!("ブラウザで開きます  {url}"))
    );
    let status = Command::new("open")
        .arg(url)
        .status()
        .context("ブラウザを開く `open` を実行できません")?;
    if !status.success() {
        bail!("ブラウザを開けませんでした（`open` が {status} で終わりました）");
    }
    Ok(())
}

/// CloudWatch Logs コンソールは URL の `#` 以降を自分で解釈するので、JavaScript の
/// encodeURIComponent をかけたうえで `%` を `$25` に置き換える。ただの `%2F` では `/` を含むロググループを開けない
fn encode_log_fragment(text: &str) -> String {
    encode_uri_component(text).replace('%', "$25")
}

/// JavaScript の encodeURIComponent と同じく、英数字と `-_.!~*'()` 以外を UTF-8 のバイトごとに `%XX` にする
fn encode_uri_component(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_url_points_to_the_task_configuration_in_the_task_region() {
        assert_eq!(
            task_url(
                "us-east-1",
                "example-staging",
                "arn:aws:ecs:us-east-1:123456789012:task/example-staging/0123456789abcdef"
            ),
            "https://us-east-1.console.aws.amazon.com/ecs/v2/clusters/example-staging/tasks/0123456789abcdef/configuration?region=us-east-1"
        );
    }

    #[test]
    fn log_stream_url_escapes_group_and_stream_the_way_the_console_expects() {
        let stream = LogStream {
            region: "ap-northeast-1".into(),
            group: "/ecs/worker".into(),
            stream: "ecs/app/0123456789abcdef".into(),
        };

        assert_eq!(
            log_stream_url(&stream),
            "https://ap-northeast-1.console.aws.amazon.com/cloudwatch/home?region=ap-northeast-1#logsV2:log-groups/log-group/$252Fecs$252Fworker/log-events/ecs$252Fapp$252F0123456789abcdef"
        );
    }

    #[test]
    fn characters_outside_encode_uri_component_are_escaped_byte_by_byte() {
        assert_eq!(encode_uri_component("a-z_0.9!~*'()"), "a-z_0.9!~*'()");
        assert_eq!(encode_uri_component("a b#c/d%"), "a%20b%23c%2Fd%25");
        assert_eq!(encode_uri_component("ログ"), "%E3%83%AD%E3%82%B0");
        assert_eq!(encode_log_fragment("a b"), "a$2520b");
    }
}
