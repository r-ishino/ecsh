use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use aws_sdk_ecs::Client;
use aws_sdk_ecs::types::Task;

use crate::ecs;
use crate::history::Finish;
use crate::logs::{LogStream, LogTail};
use crate::ui::{self, Waiting};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// DescribeTasks 1 回分から読み取った、run のタスクの進み具合
#[derive(Debug, PartialEq)]
pub enum Progress {
    /// コンテナが動き始める前。タスクの lastStatus を持つ
    Starting(String),
    /// コンテナが動き始めた後。タスクの lastStatus を持つ
    Running(String),
    Stopped(Stopped),
}

/// 止まったタスクから読み取った、コマンドの結果
#[derive(Debug, Clone, PartialEq)]
pub struct Stopped {
    /// コマンドの終了コード。起動に失敗したときなどは無い
    pub exit_code: Option<i32>,
    pub stopped_reason: Option<String>,
    /// コンテナが止まった理由（OutOfMemoryError など）
    pub container_reason: Option<String>,
}

impl Stopped {
    /// 履歴に書く形。took は起動してから止まったと分かるまで
    pub fn finish(&self, took: Duration) -> Finish {
        Finish {
            exit_code: self.exit_code,
            stopped_reason: self.stopped_reason.clone(),
            container_reason: self.container_reason.clone(),
            took_seconds: took.as_secs(),
        }
    }
}

impl From<&Finish> for Stopped {
    fn from(finish: &Finish) -> Self {
        Self {
            exit_code: finish.exit_code,
            stopped_reason: finish.stopped_reason.clone(),
            container_reason: finish.container_reason.clone(),
        }
    }
}

pub fn progress_from(task: &Task, container: &str) -> Progress {
    let status = task.last_status().unwrap_or("-");
    if status == "STOPPED" {
        let target = task
            .containers()
            .iter()
            .find(|c| c.name() == Some(container));
        return Progress::Stopped(Stopped {
            exit_code: target.and_then(|c| c.exit_code()),
            stopped_reason: task.stopped_reason().map(str::to_owned),
            container_reason: target.and_then(|c| c.reason()).map(str::to_owned),
        });
    }
    if task.started_at().is_some() {
        Progress::Running(status.to_owned())
    } else {
        Progress::Starting(status.to_owned())
    }
}

/// run のタスクを、止まるまで見届ける。出力は stdout、進み具合は stderr に出す
pub struct Follower<'a> {
    client: &'a Client,
    cluster: &'a str,
    task_arn: &'a str,
    container: &'a str,
    logs: Option<LogTail>,
    started: bool,
}

impl<'a> Follower<'a> {
    pub fn new(
        client: &'a Client,
        cluster: &'a str,
        task_arn: &'a str,
        container: &'a str,
        logs: Option<LogTail>,
    ) -> Self {
        Self {
            client,
            cluster,
            task_arn,
            container,
            logs,
            started: false,
        }
    }

    /// コマンドがもう動いているタスクを見届ける。動き始めるのを待つ表示を出さない
    pub fn already_started(self) -> Self {
        Self {
            started: true,
            ..self
        }
    }

    /// 出力が流れるはずだったのに、ログストリームが一度も見つからなかった
    pub fn missing_stream(&self) -> Option<&LogStream> {
        self.logs
            .as_ref()
            .filter(|tail| !tail.found())
            .map(LogTail::stream)
    }

    /// タスクが止まるまで待ち、止まった後に残りの出力を出し切って結果を返す
    ///
    /// シグナルで future ごと捨てられても、続きからもう一度呼べる
    pub async fn until_stopped(&mut self) -> Result<Stopped> {
        let mut waiting = self.waiting();
        loop {
            let task = ecs::describe_task(self.client, self.cluster, self.task_arn).await?;
            match progress_from(&task, self.container) {
                Progress::Stopped(stopped) => {
                    drop(waiting);
                    self.drain_after_stop().await?;
                    return Ok(stopped);
                }
                Progress::Starting(status) => {
                    if let Some(waiting) = &mut waiting {
                        waiting.update(format!("タスク {status}"));
                    }
                }
                Progress::Running(status) => {
                    if !self.started {
                        self.started = true;
                        if let Some(starting) = waiting.take() {
                            let took = ui::duration(starting.elapsed());
                            starting.finish(format!("コマンドが動き始めました（{took}）"));
                        }
                        waiting = self.waiting();
                    }
                    if let Some(waiting) = &mut waiting {
                        waiting.update(format!("タスク {status}"));
                    }
                    self.print_new().await?;
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// 出力を流している間はスピナーを出さない。stdout の行とスピナーの行が混ざる
    fn waiting(&self) -> Option<Waiting> {
        if !self.started {
            Some(Waiting::start("コマンドが動き始めるのを待っています", None))
        } else if self.logs.is_none() {
            Some(Waiting::start("コマンドが終わるのを待っています", None))
        } else {
            None
        }
    }

    /// 新しい出力を stdout に出し、その件数を返す
    async fn print_new(&mut self) -> Result<usize> {
        let Some(tail) = &mut self.logs else {
            return Ok(0);
        };
        let messages = tail.read_new().await?;
        let mut stdout = io::stdout().lock();
        for message in &messages {
            writeln!(stdout, "{message}").context("標準出力に書けません")?;
        }
        stdout.flush().context("標準出力に書けません")?;
        Ok(messages.len())
    }

    /// 止まる直前の出力は、STOPPED を見た時点ではまだ CloudWatch Logs で読めないことがあるので、新しい出力が来なくなるまで読む
    async fn drain_after_stop(&mut self) -> Result<()> {
        if self.logs.is_none() {
            return Ok(());
        }
        self.print_new().await?;
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            if self.print_new().await? == 0 {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use aws_sdk_ecs::primitives::DateTime;
    use aws_sdk_ecs::types::Container;

    use super::*;

    fn task(status: &str) -> aws_sdk_ecs::types::builders::TaskBuilder {
        Task::builder().last_status(status)
    }

    fn container(name: &str) -> aws_sdk_ecs::types::builders::ContainerBuilder {
        Container::builder().name(name)
    }

    #[test]
    fn task_is_starting_until_it_has_a_start_time() {
        assert_eq!(
            progress_from(&task("PENDING").build(), "app"),
            Progress::Starting("PENDING".into())
        );
    }

    #[test]
    fn task_is_running_once_it_has_a_start_time_even_while_stopping() {
        for status in ["RUNNING", "DEACTIVATING", "STOPPING"] {
            let task = task(status).started_at(DateTime::from_secs(0)).build();

            assert_eq!(
                progress_from(&task, "app"),
                Progress::Running(status.into())
            );
        }
    }

    #[test]
    fn stopped_task_yields_the_exit_code_and_reasons_of_the_command_container() {
        let task = task("STOPPED")
            .stopped_reason("Essential container in task exited")
            .containers(container("sidecar").exit_code(0).build())
            .containers(
                container("app")
                    .exit_code(137)
                    .reason("OutOfMemoryError")
                    .build(),
            )
            .build();

        assert_eq!(
            progress_from(&task, "app"),
            Progress::Stopped(Stopped {
                exit_code: Some(137),
                stopped_reason: Some("Essential container in task exited".into()),
                container_reason: Some("OutOfMemoryError".into()),
            })
        );
    }

    #[test]
    fn task_that_stopped_before_the_command_ran_has_no_exit_code() {
        let task = task("STOPPED")
            .stopped_reason("CannotPullContainerError: pull image manifest has been retried")
            .containers(container("app").build())
            .build();

        let Progress::Stopped(stopped) = progress_from(&task, "app") else {
            panic!("止まったと読めない");
        };
        assert_eq!(stopped.exit_code, None);
        assert!(stopped.stopped_reason.unwrap().starts_with("CannotPull"));
    }
}
