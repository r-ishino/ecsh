use std::time::Duration;

use anyhow::{Context, Result, bail};
use aws_sdk_ecs::Client;
use aws_sdk_ecs::operation::describe_tasks::DescribeTasksOutput;
use aws_sdk_ecs::types::ManagedAgentName;

use crate::ui::{self, Waiting};

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// 起動したタスクのコンテナで ExecuteCommandAgent が RUNNING になるまで待ち、そのコンテナの runtimeId を返す
pub async fn wait_until_exec_ready(
    client: &Client,
    cluster: &str,
    task_arn: &str,
    container: &str,
) -> Result<String> {
    let mut waiting = Waiting::start("入れるようになるのを待っています", Some("上限 5 分"));
    loop {
        let observation = describe(client, cluster, task_arn, container).await?;
        waiting.update(status_text(&observation));
        match next_step(&observation, waiting.elapsed())? {
            Progress::Ready => {
                let runtime_id = observation.runtime_id.with_context(|| {
                    format!(
                        "DescribeTasks の応答にコンテナ `{container}` の runtimeId がありません"
                    )
                })?;
                let took = ui::duration(waiting.elapsed());
                waiting.finish(format!("入れるようになりました（{took}）"));
                return Ok(runtime_id);
            }
            Progress::Waiting => {}
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn describe(
    client: &Client,
    cluster: &str,
    task_arn: &str,
    container: &str,
) -> Result<Observation> {
    let output = client
        .describe_tasks()
        .cluster(cluster)
        .tasks(task_arn)
        .send()
        .await
        .with_context(|| format!("DescribeTasks に失敗しました（task={task_arn}）"))?;
    observation_from(output, container)
}

/// DescribeTasks 1 回分から読み取った、待機の判定に使う状態
#[derive(Debug, Clone, PartialEq)]
struct Observation {
    task_status: String,
    desired_status: String,
    stopped_reason: Option<String>,
    /// 対象コンテナの runtimeId。session-manager-plugin に渡す接続先に要る
    runtime_id: Option<String>,
    /// 対象コンテナの managedAgents にまだ現れていなければ None
    agent: Option<Agent>,
}

#[derive(Debug, Clone, PartialEq)]
struct Agent {
    status: String,
    reason: Option<String>,
}

impl Observation {
    fn agent_status(&self) -> &str {
        self.agent.as_ref().map_or("(未起動)", |a| &a.status)
    }
}

fn observation_from(output: DescribeTasksOutput, container: &str) -> Result<Observation> {
    if let Some(failure) = output.failures().first() {
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
    let target = task
        .containers()
        .iter()
        .find(|c| c.name() == Some(container))
        .with_context(|| format!("タスクにコンテナ `{container}` がありません"))?;
    let agent = target
        .managed_agents()
        .iter()
        .find(|a| a.name() == Some(&ManagedAgentName::ExecuteCommandAgent))
        .and_then(|a| {
            Some(Agent {
                status: a.last_status()?.to_owned(),
                reason: a.reason().map(str::to_owned),
            })
        });
    Ok(Observation {
        task_status: task.last_status().unwrap_or("-").to_owned(),
        desired_status: task.desired_status().unwrap_or("-").to_owned(),
        stopped_reason: task.stopped_reason().map(str::to_owned),
        runtime_id: target.runtime_id().map(str::to_owned),
        agent,
    })
}

#[derive(Debug, PartialEq)]
enum Progress {
    Ready,
    Waiting,
}

fn next_step(observation: &Observation, elapsed: Duration) -> Result<Progress> {
    if observation.desired_status == "STOPPED" || observation.task_status == "STOPPED" {
        bail!(
            "タスクが停止しました（タスク={} stoppedReason={}）",
            observation.task_status,
            observation.stopped_reason.as_deref().unwrap_or("-")
        );
    }
    match &observation.agent {
        Some(agent) if agent.status == "RUNNING" => return Ok(Progress::Ready),
        Some(agent) if agent.status != "PENDING" => bail!(
            "ExecuteCommandAgent が {} になりました（reason={}）",
            agent.status,
            agent.reason.as_deref().unwrap_or("-")
        ),
        _ => {}
    }
    if elapsed >= TIMEOUT {
        bail!(
            "5 分待っても ExecuteCommandAgent が RUNNING になりません（タスク={} Agent={}）",
            observation.task_status,
            observation.agent_status()
        );
    }
    Ok(Progress::Waiting)
}

/// `タスク RUNNING · Agent PENDING`
fn status_text(observation: &Observation) -> String {
    format!(
        "タスク {} · Agent {}",
        observation.task_status,
        observation.agent_status()
    )
}

#[cfg(test)]
mod tests {
    use aws_sdk_ecs::types::{Container, Failure, ManagedAgent, Task};

    use super::*;

    fn observation(task_status: &str, agent_status: Option<&str>) -> Observation {
        Observation {
            task_status: task_status.into(),
            desired_status: "RUNNING".into(),
            stopped_reason: None,
            runtime_id: None,
            agent: agent_status.map(|status| Agent {
                status: status.into(),
                reason: None,
            }),
        }
    }

    fn exec_agent(status: &str) -> ManagedAgent {
        ManagedAgent::builder()
            .name(ManagedAgentName::ExecuteCommandAgent)
            .last_status(status)
            .build()
    }

    fn output_with(task: Task) -> DescribeTasksOutput {
        DescribeTasksOutput::builder().tasks(task).build()
    }

    #[test]
    fn exec_agent_and_runtime_id_of_target_container_are_observed() {
        let task = Task::builder()
            .last_status("RUNNING")
            .desired_status("RUNNING")
            .containers(
                Container::builder()
                    .name("sidecar")
                    .managed_agents(exec_agent("PENDING"))
                    .build(),
            )
            .containers(
                Container::builder()
                    .name("app")
                    .runtime_id("abc-123")
                    .managed_agents(exec_agent("RUNNING"))
                    .build(),
            )
            .build();

        let observed = observation_from(output_with(task), "app").unwrap();

        assert_eq!(
            observed,
            Observation {
                runtime_id: Some("abc-123".into()),
                ..observation("RUNNING", Some("RUNNING"))
            }
        );
    }

    #[test]
    fn container_without_exec_agent_yet_is_observed_as_not_started() {
        let task = Task::builder()
            .last_status("PROVISIONING")
            .desired_status("RUNNING")
            .containers(Container::builder().name("app").build())
            .build();

        let observed = observation_from(output_with(task), "app").unwrap();

        assert_eq!(observed.agent, None);
        assert_eq!(observed.agent_status(), "(未起動)");
    }

    #[test]
    fn task_without_target_container_is_rejected() {
        let task = Task::builder()
            .last_status("RUNNING")
            .containers(Container::builder().name("sidecar").build())
            .build();

        let message = observation_from(output_with(task), "app")
            .unwrap_err()
            .to_string();

        assert!(message.contains("`app`"));
    }

    #[test]
    fn describe_tasks_failure_error_carries_reason() {
        let output = DescribeTasksOutput::builder()
            .failures(Failure::builder().reason("MISSING").build())
            .build();

        let message = observation_from(output, "app").unwrap_err().to_string();

        assert!(message.contains("MISSING"));
    }

    #[test]
    fn running_exec_agent_is_ready() {
        let step = next_step(&observation("RUNNING", Some("RUNNING")), Duration::ZERO);

        assert_eq!(step.unwrap(), Progress::Ready);
    }

    #[test]
    fn pending_or_not_started_agent_keeps_waiting() {
        for agent in [None, Some("PENDING")] {
            let step = next_step(&observation("PENDING", agent), Duration::from_secs(10));

            assert_eq!(step.unwrap(), Progress::Waiting, "Agent={agent:?}");
        }
    }

    #[test]
    fn task_going_to_stop_fails_with_stopped_reason_before_timeout() {
        let stopping = Observation {
            task_status: "DEACTIVATING".into(),
            desired_status: "STOPPED".into(),
            stopped_reason: Some("Essential container in task exited".into()),
            runtime_id: None,
            agent: None,
        };

        let message = next_step(&stopping, Duration::from_secs(10))
            .unwrap_err()
            .to_string();

        assert!(message.contains("Essential container in task exited"));
    }

    #[test]
    fn stopped_or_unknown_agent_status_fails_with_status_and_reason() {
        for status in ["STOPPED", "FAILED"] {
            let mut observed = observation("RUNNING", Some(status));
            observed.agent.as_mut().unwrap().reason = Some("agent crashed".into());

            let message = next_step(&observed, Duration::from_secs(10))
                .unwrap_err()
                .to_string();

            assert!(message.contains(status));
            assert!(message.contains("agent crashed"));
        }
    }

    #[test]
    fn waiting_fails_after_5_minutes_with_last_task_and_agent_status() {
        let observed = observation("RUNNING", Some("PENDING"));

        assert!(next_step(&observed, TIMEOUT - Duration::from_secs(1)).is_ok());
        let message = next_step(&observed, TIMEOUT).unwrap_err().to_string();

        assert!(message.contains("タスク=RUNNING"));
        assert!(message.contains("Agent=PENDING"));
    }

    #[test]
    fn agent_becoming_running_at_the_deadline_is_still_ready() {
        let step = next_step(&observation("RUNNING", Some("RUNNING")), TIMEOUT);

        assert_eq!(step.unwrap(), Progress::Ready);
    }

    #[test]
    fn status_shows_task_and_agent_status() {
        assert_eq!(
            status_text(&observation("PENDING", None)),
            "タスク PENDING · Agent (未起動)"
        );
        assert_eq!(
            status_text(&observation("RUNNING", Some("PENDING"))),
            "タスク RUNNING · Agent PENDING"
        );
    }
}
