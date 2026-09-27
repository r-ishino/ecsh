//! コンテナの出力の送り先を、タスク定義のログ設定から求め、CloudWatch Logs から読む

use std::fmt;

use anyhow::{Context, Result};
use aws_sdk_cloudwatchlogs::Client;
use aws_sdk_ecs::types::{LogConfiguration, LogDriver};

use crate::aws_profile::AwsProfile;

/// awslogs ドライバがコンテナの出力を書くログストリーム
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStream {
    pub region: String,
    pub group: String,
    pub stream: String,
}

impl fmt::Display for LogStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} / {}", self.group, self.stream)
    }
}

/// コンテナの出力を CloudWatch Logs から読めるかどうか
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogDestination {
    Awslogs(LogStream),
    /// 読めない。理由は「〜ので」に続けられる形
    Unreadable(String),
}

/// コンテナのログ設定から、タスク `task_id` の出力が書かれるログストリームを求める
///
/// `default_region` は `awslogs-region` が無いときの region。awslogs ドライバはタスクと同じ region に送る
pub fn destination(
    log_configuration: Option<&LogConfiguration>,
    container: &str,
    task_id: &str,
    default_region: &str,
) -> LogDestination {
    let Some(configuration) = log_configuration else {
        return LogDestination::Unreadable("コンテナにログ設定が無い".into());
    };
    if configuration.log_driver() != &LogDriver::Awslogs {
        return LogDestination::Unreadable(format!(
            "ログドライバが awslogs ではなく {} な",
            configuration.log_driver().as_str()
        ));
    }
    let option = |key: &str| {
        configuration
            .options()
            .and_then(|options| options.get(key))
            .filter(|value| !value.is_empty())
    };
    let Some(group) = option("awslogs-group") else {
        return LogDestination::Unreadable("ログ設定に awslogs-group が無い".into());
    };
    // prefix が無いとストリーム名が Docker のコンテナ ID になり、ECS の API からは求められない
    let Some(prefix) = option("awslogs-stream-prefix") else {
        return LogDestination::Unreadable(
            "ログ設定に awslogs-stream-prefix が無く、ログストリームの名前が決まらない".into(),
        );
    };
    LogDestination::Awslogs(LogStream {
        region: option("awslogs-region")
            .map_or(default_region, String::as_str)
            .to_owned(),
        group: group.clone(),
        stream: format!("{prefix}/{container}/{task_id}"),
    })
}

pub async fn client(region: &str, aws_profile: &AwsProfile) -> Client {
    Client::new(&aws_profile.sdk_config(region).await)
}

/// 1 つのログストリームを、前回読んだところから続けて読む
pub struct LogTail {
    client: Client,
    stream: LogStream,
    next_token: Option<String>,
    found: bool,
}

impl LogTail {
    pub fn new(client: Client, stream: LogStream) -> Self {
        Self {
            client,
            stream,
            next_token: None,
            found: false,
        }
    }

    pub fn stream(&self) -> &LogStream {
        &self.stream
    }

    /// ログストリームが一度でも見つかったか。コンテナが動き始めるまではストリームが無い
    pub fn found(&self) -> bool {
        self.found
    }

    /// 前回の続きから、今読めるメッセージをすべて返す。ストリームがまだ無ければ空
    pub async fn read_new(&mut self) -> Result<Vec<String>> {
        let mut messages = Vec::new();
        loop {
            let sent_token = self.next_token.clone();
            let response = self
                .client
                .get_log_events()
                .log_group_name(&self.stream.group)
                .log_stream_name(&self.stream.stream)
                .start_from_head(true)
                .set_next_token(sent_token.clone())
                .send()
                .await;
            let output = match response {
                Ok(output) => output,
                Err(error)
                    if error
                        .as_service_error()
                        .is_some_and(|e| e.is_resource_not_found_exception()) =>
                {
                    return Ok(messages);
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("GetLogEvents に失敗しました（{}）", self.stream)
                    });
                }
            };
            self.found = true;
            messages.extend(
                output
                    .events()
                    .iter()
                    .filter_map(|event| event.message().map(str::to_owned)),
            );
            let next_token = output.next_forward_token().map(str::to_owned);
            // 末尾まで読むと、送ったのと同じトークンが返る
            if next_token.is_none() || next_token == sent_token {
                return Ok(messages);
            }
            self.next_token = next_token;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASK_ID: &str = "0123456789abcdef";

    fn awslogs(options: &[(&str, &str)]) -> LogConfiguration {
        options
            .iter()
            .fold(
                LogConfiguration::builder().log_driver(LogDriver::Awslogs),
                |builder, (key, value)| builder.options(*key, *value),
            )
            .build()
            .unwrap()
    }

    fn destination_of(configuration: Option<&LogConfiguration>) -> LogDestination {
        destination(configuration, "app", TASK_ID, "us-east-1")
    }

    #[test]
    fn awslogs_stream_is_prefix_container_and_task_id_in_the_task_region() {
        let configuration = awslogs(&[
            ("awslogs-group", "/ecs/worker"),
            ("awslogs-stream-prefix", "ecs"),
        ]);

        assert_eq!(
            destination_of(Some(&configuration)),
            LogDestination::Awslogs(LogStream {
                region: "us-east-1".into(),
                group: "/ecs/worker".into(),
                stream: format!("ecs/app/{TASK_ID}"),
            })
        );
    }

    #[test]
    fn awslogs_region_option_overrides_the_task_region() {
        let configuration = awslogs(&[
            ("awslogs-group", "/ecs/worker"),
            ("awslogs-stream-prefix", "ecs"),
            ("awslogs-region", "ap-northeast-1"),
        ]);

        let LogDestination::Awslogs(stream) = destination_of(Some(&configuration)) else {
            panic!("awslogs として読めない");
        };
        assert_eq!(stream.region, "ap-northeast-1");
    }

    #[test]
    fn output_is_unreadable_without_awslogs_group_and_stream_prefix() {
        let cases = [
            (None, "ログ設定が無い"),
            (
                Some(
                    LogConfiguration::builder()
                        .log_driver(LogDriver::Awsfirelens)
                        .build()
                        .unwrap(),
                ),
                "awsfirelens",
            ),
            (
                Some(awslogs(&[("awslogs-stream-prefix", "ecs")])),
                "awslogs-group",
            ),
            (
                Some(awslogs(&[("awslogs-group", "/ecs/worker")])),
                "awslogs-stream-prefix",
            ),
        ];

        for (configuration, reason) in cases {
            match destination_of(configuration.as_ref()) {
                LogDestination::Unreadable(message) => {
                    assert!(message.contains(reason), "{message}")
                }
                readable => panic!("{reason}: 読めることになっている: {readable:?}"),
            }
        }
    }

    #[test]
    fn log_stream_is_shown_as_group_and_stream() {
        let stream = LogStream {
            region: "us-east-1".into(),
            group: "/ecs/worker".into(),
            stream: "ecs/app/abc".into(),
        };

        assert_eq!(stream.to_string(), "/ecs/worker / ecs/app/abc");
    }
}
