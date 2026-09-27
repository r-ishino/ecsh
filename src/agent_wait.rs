use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use aws_sdk_ecs::Client;
use aws_sdk_ecs::operation::describe_tasks::DescribeTasksOutput;
use aws_sdk_ecs::types::ManagedAgentName;

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// 起動したタスクのコンテナで ExecuteCommandAgent が RUNNING になるまで待つ
pub async fn wait_until_exec_ready(
    client: &Client,
    cluster: &str,
    task_arn: &str,
    container: &str,
) -> Result<()> {
    eprintln!("ExecuteCommandAgent の起動を待っています（上限 5 分）");
    let started = Instant::now();
    let mut previous: Option<Observation> = None;
    loop {
        let observation = describe(client, cluster, task_arn, container).await?;
        let elapsed = started.elapsed();
        if previous
            .as_ref()
            .is_none_or(|p| p.status_changed(&observation))
        {
            eprintln!("{}", status_line(elapsed, &observation));
        }
        match next_step(&observation, elapsed)? {
            Progress::Ready => {
                eprintln!("ExecuteCommandAgent が RUNNING になりました");
                return Ok(());
            }
            Progress::Waiting => {}
        }
        previous = Some(observation);
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

    fn status_changed(&self, current: &Observation) -> bool {
        self.task_status != current.task_status || self.agent_status() != current.agent_status()
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

/// `[00:21] タスク=PENDING Agent=(未起動)`
fn status_line(elapsed: Duration, observation: &Observation) -> String {
    let seconds = elapsed.as_secs();
    format!(
        "[{:02}:{:02}] タスク={} Agent={}",
        seconds / 60,
        seconds % 60,
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
    fn exec_agent_of_target_container_is_observed() {
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
                    .managed_agents(exec_agent("RUNNING"))
                    .build(),
            )
            .build();

        let observed = observation_from(output_with(task), "app").unwrap();

        assert_eq!(observed, observation("RUNNING", Some("RUNNING")));
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
    fn change_in_task_or_agent_status_is_detected() {
        let before = observation("PENDING", None);

        assert!(before.status_changed(&observation("RUNNING", None)));
        assert!(before.status_changed(&observation("PENDING", Some("PENDING"))));
        assert!(!before.status_changed(&observation("PENDING", None)));
    }

    #[test]
    fn status_line_shows_elapsed_minutes_and_seconds_with_task_and_agent_status() {
        assert_eq!(
            status_line(Duration::from_secs(21), &observation("PENDING", None)),
            "[00:21] タスク=PENDING Agent=(未起動)"
        );
        assert_eq!(
            status_line(
                Duration::from_millis(125_900),
                &observation("RUNNING", Some("PENDING"))
            ),
            "[02:05] タスク=RUNNING Agent=PENDING"
        );
    }
}
