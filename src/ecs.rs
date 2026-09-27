use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use aws_sdk_ecs::Client;
use aws_sdk_ecs::operation::describe_services::DescribeServicesOutput;
use aws_sdk_ecs::operation::describe_tasks::DescribeTasksOutput;
use aws_sdk_ecs::operation::run_task::RunTaskOutput;
use aws_sdk_ecs::operation::run_task::builders::RunTaskFluentBuilder;
use aws_sdk_ecs::operation::stop_task::builders::StopTaskFluentBuilder;
use aws_sdk_ecs::primitives::DateTime;
use aws_sdk_ecs::types::{
    AwsVpcConfiguration, CapacityProviderStrategyItem, ContainerDefinition, ContainerOverride,
    LaunchType, NetworkConfiguration, Tag, Task, TaskField, TaskOverride,
};

use crate::aws_profile::AwsProfile;

pub async fn client(region: &str, aws_profile: &AwsProfile) -> Client {
    Client::new(&aws_profile.sdk_config(region).await)
}

/// 使い捨てタスクを起動するときにサービスから写す設定
#[derive(Debug, PartialEq)]
pub struct ServiceSnapshot {
    /// RunTask にファミリー名だけを渡し、登録済みの最新リビジョンで起動させる
    pub task_family: String,
    pub network: AwsVpcConfiguration,
    pub launch_type: Option<LaunchType>,
    pub capacity_provider_strategy: Vec<CapacityProviderStrategyItem>,
    pub platform_version: Option<String>,
}

pub async fn describe_service(
    client: &Client,
    cluster: &str,
    service: &str,
) -> Result<ServiceSnapshot> {
    let output = client
        .describe_services()
        .cluster(cluster)
        .services(service)
        .send()
        .await
        .with_context(|| {
            format!("DescribeServices に失敗しました（cluster={cluster} service={service}）")
        })?;
    snapshot_from(output, cluster, service)
}

fn snapshot_from(
    output: DescribeServicesOutput,
    cluster: &str,
    service: &str,
) -> Result<ServiceSnapshot> {
    if let Some(failure) = output.failures().first() {
        match failure.reason() {
            Some("MISSING") => bail!("クラスタ `{cluster}` にサービス `{service}` がありません"),
            reason => bail!(
                "サービス `{service}` を取得できません（reason={} detail={}）",
                reason.unwrap_or("-"),
                failure.detail().unwrap_or("-")
            ),
        }
    }
    let found = output
        .services()
        .first()
        .with_context(|| format!("DescribeServices の応答にサービス `{service}` がありません"))?;

    // 削除済み（INACTIVE）や削除中（DRAINING）のサービスも failures ではなく services に入って返る
    let status = found.status().unwrap_or("-");
    if status != "ACTIVE" {
        bail!("サービス `{service}` は ACTIVE ではありません（status={status}）");
    }

    let task_definition = found
        .task_definition()
        .with_context(|| format!("サービス `{service}` にタスク定義がありません"))?;
    let network = found
        .network_configuration()
        .and_then(|n| n.awsvpc_configuration())
        .with_context(|| {
            format!(
                "サービス `{service}` に awsvpc のネットワーク設定がありません（ecsh は awsvpc ネットワークモードのサービスだけを扱います）"
            )
        })?;

    Ok(ServiceSnapshot {
        task_family: family_of(task_definition)?.to_owned(),
        network: network.clone(),
        launch_type: found.launch_type().cloned(),
        capacity_provider_strategy: found.capacity_provider_strategy().to_vec(),
        platform_version: found.platform_version().map(str::to_owned),
    })
}

/// `arn:aws:ecs:<region>:<account>:task-definition/worker:42` → `worker`
fn family_of(task_definition_arn: &str) -> Result<&str> {
    task_definition_arn
        .rsplit_once('/')
        .and_then(|(_, family_revision)| family_revision.rsplit_once(':'))
        .map(|(family, _)| family)
        .filter(|family| !family.is_empty())
        .with_context(|| {
            format!("タスク定義の ARN からファミリー名を取り出せません: {task_definition_arn}")
        })
}

/// 使い捨てタスクのコンテナのコマンド。ecsh が止め損ねても、12 時間で終了してタスクが止まる
pub const KEEPALIVE_COMMAND: [&str; 2] = ["sleep", "43200"];

/// `KEEPALIVE_COMMAND` の sleep の秒数
pub const KEEPALIVE_SECONDS: u64 = 43_200;

/// ecsh が起動したタスクの startedBy。ps / gc はこの値で自分のタスクを絞り込む
pub fn started_by(user: &str) -> Result<String> {
    let started_by = format!("ecsh/{user}");
    if user.is_empty()
        || !user
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | '_'))
    {
        bail!(
            "ユーザー名 `{user}` は startedBy に使えません（使える文字は英数字・`-`・`/`・`_` だけです）"
        );
    }
    if started_by.len() > 128 {
        bail!("startedBy `{started_by}` が ECS の上限の 128 文字を超えています");
    }
    Ok(started_by)
}

/// RunTask が起動したタスク
#[derive(Debug, PartialEq)]
pub struct LaunchedTask {
    pub task_arn: String,
    /// 実際に使われたリビジョンまで含むタスク定義の ARN
    pub task_definition_arn: String,
}

/// 使い捨てタスクのコンテナで何を動かすか
#[derive(Debug, Clone, Copy)]
pub enum Workload<'a> {
    /// exec で入る。コンテナは `KEEPALIVE_COMMAND` で待たせておく
    Shell,
    /// run で流す。シェルを通さず、配列のままコンテナのコマンドにする
    Command(&'a [String]),
}

pub async fn run_task(
    client: &Client,
    cluster: &str,
    container: &str,
    snapshot: &ServiceSnapshot,
    started_by: &str,
    workload: Workload<'_>,
) -> Result<LaunchedTask> {
    let output = run_task_request(client, cluster, container, snapshot, started_by, workload)
        .send()
        .await
        .with_context(|| format!("RunTask に失敗しました（cluster={cluster}）"))?;
    launched_from(output)
}

fn run_task_request(
    client: &Client,
    cluster: &str,
    container: &str,
    snapshot: &ServiceSnapshot,
    started_by: &str,
    workload: Workload<'_>,
) -> RunTaskFluentBuilder {
    let (command, tags) = match workload {
        Workload::Shell => (KEEPALIVE_COMMAND.map(String::from).to_vec(), None),
        Workload::Command(command) => (
            command.to_vec(),
            Some(vec![
                Tag::builder().key(MODE_TAG_KEY).value(RUN_MODE).build(),
            ]),
        ),
    };
    let command_override = ContainerOverride::builder()
        .name(container)
        .set_command(Some(command))
        .build();
    client
        .run_task()
        .cluster(cluster)
        .task_definition(&snapshot.task_family)
        .set_launch_type(snapshot.launch_type.clone())
        .set_capacity_provider_strategy(Some(snapshot.capacity_provider_strategy.clone()))
        .set_platform_version(snapshot.platform_version.clone())
        .network_configuration(
            NetworkConfiguration::builder()
                .awsvpc_configuration(snapshot.network.clone())
                .build(),
        )
        .enable_execute_command(matches!(workload, Workload::Shell))
        .started_by(started_by)
        .set_tags(tags)
        .overrides(
            TaskOverride::builder()
                .container_overrides(command_override)
                .build(),
        )
}

fn launched_from(output: RunTaskOutput) -> Result<LaunchedTask> {
    if let Some(failure) = output.failures().first() {
        bail!(
            "タスクを起動できません（reason={} detail={}）",
            failure.reason().unwrap_or("-"),
            failure.detail().unwrap_or("-")
        );
    }
    let task = output
        .tasks()
        .first()
        .context("RunTask の応答に起動したタスクがありません")?;
    Ok(LaunchedTask {
        task_arn: task
            .task_arn()
            .context("RunTask の応答にタスクの ARN がありません")?
            .to_owned(),
        task_definition_arn: task
            .task_definition_arn()
            .context("RunTask の応答にタスク定義の ARN がありません")?
            .to_owned(),
    })
}

/// exec が抜けたときや、run を Ctrl-C で止めると答えたときの StopTask の reason。コンソールのタスクの停止理由に出る
pub const STOP_REASON: &str = "Stopped by ecsh";
/// gc が止めたときの StopTask の reason
pub const GC_STOP_REASON: &str = "ecsh gc";

pub async fn stop_task(client: &Client, cluster: &str, task_arn: &str, reason: &str) -> Result<()> {
    stop_task_request(client, cluster, task_arn, reason)
        .send()
        .await
        .with_context(|| format!("StopTask に失敗しました（cluster={cluster}）"))?;
    Ok(())
}

fn stop_task_request(
    client: &Client,
    cluster: &str,
    task_arn: &str,
    reason: &str,
) -> StopTaskFluentBuilder {
    client
        .stop_task()
        .cluster(cluster)
        .task(task_arn)
        .reason(reason)
}

/// 1 つのタスクの今の状態を DescribeTasks で取る
pub async fn describe_task(client: &Client, cluster: &str, task_arn: &str) -> Result<Task> {
    let output = client
        .describe_tasks()
        .cluster(cluster)
        .tasks(task_arn)
        .send()
        .await
        .with_context(|| format!("DescribeTasks に失敗しました（task={task_arn}）"))?;
    task_from(output)
}

fn task_from(output: DescribeTasksOutput) -> Result<Task> {
    if let Some(failure) = output.failures().first() {
        bail!(
            "タスクの状態を取得できません（reason={} detail={}）",
            failure.reason().unwrap_or("-"),
            failure.detail().unwrap_or("-")
        );
    }
    output
        .tasks()
        .first()
        .cloned()
        .context("DescribeTasks の応答にタスクがありません")
}

/// タスク定義（リビジョンまで含む ARN）から、コンテナ `container` の定義を取る
pub async fn container_definition(
    client: &Client,
    task_definition_arn: &str,
    container: &str,
) -> Result<ContainerDefinition> {
    let output = client
        .describe_task_definition()
        .task_definition(task_definition_arn)
        .send()
        .await
        .with_context(|| {
            format!("DescribeTaskDefinition に失敗しました（{task_definition_arn}）")
        })?;
    output
        .task_definition()
        .and_then(|definition| {
            definition
                .container_definitions()
                .iter()
                .find(|c| c.name() == Some(container))
        })
        .cloned()
        .with_context(|| {
            format!("タスク定義 {task_definition_arn} にコンテナ `{container}` がありません")
        })
}

/// run が流しているタスクに付くタグ。exec のタスクには付かない
const MODE_TAG_KEY: &str = "ecsh:mode";
const RUN_MODE: &str = "run";

/// DescribeTasks に一度に渡せるタスクの上限
const DESCRIBE_TASKS_LIMIT: usize = 100;

/// ecsh が起動して、まだ止める指示を受けていないタスク
#[derive(Debug, Clone, PartialEq)]
pub struct OwnTask {
    pub task_arn: String,
    pub last_status: String,
    pub task_definition_arn: String,
    /// RunTask を受け付けた時刻
    pub created_at: Option<SystemTime>,
    /// コンテナが動き始めた時刻。12 時間の sleep はここから数える。PENDING の間は無い
    pub started_at: Option<SystemTime>,
    /// run が流しているタスク。接続しないのが正常
    pub is_run: bool,
}

/// クラスタで startedBy が `started_by` のタスクを、ListTasks → DescribeTasks で取る
pub async fn list_own_tasks(
    client: &Client,
    cluster: &str,
    started_by: &str,
) -> Result<Vec<OwnTask>> {
    let mut task_arns = Vec::new();
    let mut next_token = None;
    loop {
        // startedBy を指定するとほかの絞り込み条件と併用できないので、クラスタと startedBy だけで絞る
        let output = client
            .list_tasks()
            .cluster(cluster)
            .started_by(started_by)
            .set_next_token(next_token)
            .send()
            .await
            .with_context(|| format!("ListTasks に失敗しました（cluster={cluster}）"))?;
        task_arns.extend(output.task_arns().iter().cloned());
        next_token = output.next_token().map(str::to_owned);
        if next_token.is_none() {
            break;
        }
    }

    let mut tasks = Vec::with_capacity(task_arns.len());
    for chunk in task_arns.chunks(DESCRIBE_TASKS_LIMIT) {
        let output = client
            .describe_tasks()
            .cluster(cluster)
            .set_tasks(Some(chunk.to_vec()))
            .include(TaskField::Tags)
            .send()
            .await
            .with_context(|| format!("DescribeTasks に失敗しました（cluster={cluster}）"))?;
        tasks.extend(own_tasks_from(output)?);
    }
    Ok(tasks)
}

fn own_tasks_from(output: DescribeTasksOutput) -> Result<Vec<OwnTask>> {
    // ListTasks の後に止まって消えたタスクは MISSING で返る。もう動いていないので一覧から外すだけでよい
    if let Some(failure) = output
        .failures()
        .iter()
        .find(|failure| failure.reason() != Some("MISSING"))
    {
        bail!(
            "タスクを取得できません（arn={} reason={} detail={}）",
            failure.arn().unwrap_or("-"),
            failure.reason().unwrap_or("-"),
            failure.detail().unwrap_or("-")
        );
    }
    output.tasks().iter().map(own_task_from).collect()
}

fn own_task_from(task: &Task) -> Result<OwnTask> {
    let task_arn = task
        .task_arn()
        .context("DescribeTasks の応答にタスクの ARN がありません")?;
    Ok(OwnTask {
        task_arn: task_arn.to_owned(),
        last_status: task.last_status().unwrap_or("-").to_owned(),
        task_definition_arn: task
            .task_definition_arn()
            .with_context(|| format!("タスク {task_arn} にタスク定義の ARN がありません"))?
            .to_owned(),
        created_at: task.created_at().and_then(system_time),
        started_at: task.started_at().and_then(system_time),
        is_run: task
            .tags()
            .iter()
            .any(|tag| tag.key() == Some(MODE_TAG_KEY) && tag.value() == Some(RUN_MODE)),
    })
}

fn system_time(date_time: &DateTime) -> Option<SystemTime> {
    SystemTime::try_from(*date_time).ok()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aws_config::{BehaviorVersion, Region};
    use aws_sdk_ecs::types::{AssignPublicIp, Failure, Service, Tag};

    use super::*;

    const TASK_DEFINITION: &str = "arn:aws:ecs:us-east-1:123456789012:task-definition/worker:42";

    fn awsvpc() -> AwsVpcConfiguration {
        AwsVpcConfiguration::builder()
            .subnets("subnet-a")
            .subnets("subnet-b")
            .security_groups("sg-1")
            .assign_public_ip(AssignPublicIp::Disabled)
            .build()
            .unwrap()
    }

    fn active_service() -> aws_sdk_ecs::types::builders::ServiceBuilder {
        Service::builder()
            .status("ACTIVE")
            .task_definition(TASK_DEFINITION)
            .launch_type(LaunchType::Fargate)
            .platform_version("LATEST")
            .network_configuration(
                NetworkConfiguration::builder()
                    .awsvpc_configuration(awsvpc())
                    .build(),
            )
    }

    fn output_with(service: Service) -> DescribeServicesOutput {
        DescribeServicesOutput::builder().services(service).build()
    }

    fn snapshot() -> ServiceSnapshot {
        ServiceSnapshot {
            task_family: "worker".into(),
            network: awsvpc(),
            launch_type: Some(LaunchType::Fargate),
            capacity_provider_strategy: vec![],
            platform_version: Some("LATEST".into()),
        }
    }

    #[test]
    fn active_service_yields_task_family_launch_settings_and_awsvpc_config() {
        let snapshot = snapshot_from(output_with(active_service().build()), "c", "worker").unwrap();

        assert_eq!(snapshot, self::snapshot());
    }

    #[test]
    fn capacity_provider_strategy_is_copied_from_service() {
        let strategy = CapacityProviderStrategyItem::builder()
            .capacity_provider("FARGATE")
            .weight(1)
            .build()
            .unwrap();
        let service = Service::builder()
            .status("ACTIVE")
            .task_definition(TASK_DEFINITION)
            .capacity_provider_strategy(strategy.clone())
            .network_configuration(
                NetworkConfiguration::builder()
                    .awsvpc_configuration(awsvpc())
                    .build(),
            )
            .build();

        let snapshot = snapshot_from(output_with(service), "c", "worker").unwrap();

        assert_eq!(snapshot.launch_type, None);
        assert_eq!(snapshot.capacity_provider_strategy, vec![strategy]);
    }

    #[test]
    fn family_is_taken_from_task_definition_arn() {
        assert_eq!(family_of(TASK_DEFINITION).unwrap(), "worker");
    }

    #[test]
    fn task_definition_arn_without_revision_is_rejected() {
        let arn = "arn:aws:ecs:us-east-1:123456789012:task-definition/worker";

        assert!(family_of(arn).is_err());
    }

    #[test]
    fn started_by_prefixes_user_with_ecsh() {
        assert_eq!(started_by("r-ishino_2").unwrap(), "ecsh/r-ishino_2");
    }

    #[test]
    fn user_with_characters_not_allowed_in_started_by_is_rejected() {
        for user in ["first.last", "user@example", "名前", ""] {
            assert!(started_by(user).is_err(), "{user:?} は受け付けない");
        }
    }

    #[test]
    fn started_by_longer_than_128_characters_is_rejected() {
        let longest = "a".repeat(128 - "ecsh/".len());

        assert!(started_by(&longest).is_ok());
        assert!(started_by(&format!("{longest}a")).is_err());
    }

    fn offline_client() -> Client {
        Client::from_conf(
            aws_sdk_ecs::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .region(Region::new("us-east-1"))
                .build(),
        )
    }

    #[test]
    fn run_task_request_launches_latest_revision_like_the_service() {
        let request = run_task_request(
            &offline_client(),
            "c",
            "app",
            &snapshot(),
            "ecsh/me",
            Workload::Shell,
        );
        let input = request.as_input();

        assert_eq!(input.get_cluster().as_deref(), Some("c"));
        assert_eq!(input.get_task_definition().as_deref(), Some("worker"));
        assert_eq!(input.get_launch_type(), &Some(LaunchType::Fargate));
        assert_eq!(input.get_platform_version().as_deref(), Some("LATEST"));
        assert_eq!(
            input
                .get_network_configuration()
                .as_ref()
                .and_then(|n| n.awsvpc_configuration()),
            Some(&awsvpc())
        );
    }

    #[test]
    fn run_task_request_enables_exec_and_tags_the_task_as_ecsh() {
        let request = run_task_request(
            &offline_client(),
            "c",
            "app",
            &snapshot(),
            "ecsh/me",
            Workload::Shell,
        );
        let input = request.as_input();

        assert_eq!(input.get_enable_execute_command(), &Some(true));
        assert_eq!(input.get_started_by().as_deref(), Some("ecsh/me"));
    }

    #[test]
    fn run_task_request_replaces_container_command_with_12_hour_sleep() {
        let request = run_task_request(
            &offline_client(),
            "c",
            "app",
            &snapshot(),
            "ecsh/me",
            Workload::Shell,
        );
        let overrides = request
            .as_input()
            .get_overrides()
            .as_ref()
            .unwrap()
            .container_overrides();

        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].name(), Some("app"));
        assert_eq!(overrides[0].command(), ["sleep", "43200"]);
    }

    fn run_request(command: &[String]) -> RunTaskFluentBuilder {
        run_task_request(
            &offline_client(),
            "c",
            "app",
            &snapshot(),
            "ecsh/me",
            Workload::Command(command),
        )
    }

    #[test]
    fn run_passes_the_command_as_is_as_the_container_command_without_a_shell() {
        let command: Vec<String> = ["bundle", "exec", "rake", "task[a, b]", "KEY=a b"]
            .map(String::from)
            .to_vec();
        let request = run_request(&command);
        let overrides = request
            .as_input()
            .get_overrides()
            .as_ref()
            .unwrap()
            .container_overrides();

        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].name(), Some("app"));
        assert_eq!(overrides[0].command(), command.as_slice());
    }

    #[test]
    fn run_launches_like_exec_with_the_same_started_by_but_tagged_as_run() {
        let request = run_request(&["true".into()]);
        let input = request.as_input();

        assert_eq!(input.get_task_definition().as_deref(), Some("worker"));
        assert_eq!(input.get_launch_type(), &Some(LaunchType::Fargate));
        assert_eq!(input.get_started_by().as_deref(), Some("ecsh/me"));
        assert_eq!(
            input.get_tags().as_deref(),
            Some([Tag::builder().key("ecsh:mode").value("run").build()].as_slice())
        );
    }

    #[test]
    fn run_does_not_enable_exec_but_exec_does_and_is_not_tagged() {
        let run = run_request(&["true".into()]);
        let exec = run_task_request(
            &offline_client(),
            "c",
            "app",
            &snapshot(),
            "ecsh/me",
            Workload::Shell,
        );

        assert_eq!(run.as_input().get_enable_execute_command(), &Some(false));
        assert_eq!(exec.as_input().get_enable_execute_command(), &Some(true));
        assert_eq!(exec.as_input().get_tags(), &None);
    }

    #[test]
    fn launched_task_carries_task_arn_and_used_revision() {
        let output = RunTaskOutput::builder()
            .tasks(
                Task::builder()
                    .task_arn("arn:aws:ecs:us-east-1:123456789012:task/c/abc")
                    .task_definition_arn(TASK_DEFINITION)
                    .build(),
            )
            .build();

        assert_eq!(
            launched_from(output).unwrap(),
            LaunchedTask {
                task_arn: "arn:aws:ecs:us-east-1:123456789012:task/c/abc".into(),
                task_definition_arn: TASK_DEFINITION.into(),
            }
        );
    }

    #[test]
    fn run_task_failure_error_carries_reason_and_detail() {
        let output = RunTaskOutput::builder()
            .failures(
                Failure::builder()
                    .reason("RESOURCE:MEMORY")
                    .detail("not enough memory")
                    .build(),
            )
            .build();

        let message = launched_from(output).unwrap_err().to_string();

        assert!(message.contains("RESOURCE:MEMORY"));
        assert!(message.contains("not enough memory"));
    }

    #[test]
    fn stop_task_request_stops_the_launched_task_with_ecsh_as_reason() {
        let task_arn = "arn:aws:ecs:us-east-1:123456789012:task/c/abc";
        let request = stop_task_request(&offline_client(), "c", task_arn, STOP_REASON);
        let input = request.as_input();

        assert_eq!(input.get_cluster().as_deref(), Some("c"));
        assert_eq!(input.get_task().as_deref(), Some(task_arn));
        assert_eq!(input.get_reason().as_deref(), Some("Stopped by ecsh"));
    }

    #[test]
    fn gc_stops_tasks_with_ecsh_gc_as_reason() {
        let task_arn = "arn:aws:ecs:us-east-1:123456789012:task/c/abc";
        let request = stop_task_request(&offline_client(), "c", task_arn, GC_STOP_REASON);

        assert_eq!(request.as_input().get_reason().as_deref(), Some("ecsh gc"));
    }

    #[test]
    fn missing_service_error_names_cluster_and_service() {
        let output = DescribeServicesOutput::builder()
            .failures(Failure::builder().reason("MISSING").build())
            .build();

        let message = snapshot_from(output, "example-staging", "worker")
            .unwrap_err()
            .to_string();

        assert!(message.contains("`example-staging`"));
        assert!(message.contains("`worker`"));
    }

    #[test]
    fn other_failure_error_carries_reason_and_detail() {
        let output = DescribeServicesOutput::builder()
            .failures(
                Failure::builder()
                    .reason("UNKNOWN")
                    .detail("something went wrong")
                    .build(),
            )
            .build();

        let message = snapshot_from(output, "c", "worker")
            .unwrap_err()
            .to_string();

        assert!(message.contains("UNKNOWN"));
        assert!(message.contains("something went wrong"));
    }

    #[test]
    fn inactive_service_is_rejected() {
        let output = output_with(active_service().status("INACTIVE").build());

        let message = snapshot_from(output, "c", "worker")
            .unwrap_err()
            .to_string();

        assert!(message.contains("INACTIVE"));
    }

    #[test]
    fn service_without_awsvpc_config_is_rejected() {
        let service = Service::builder()
            .status("ACTIVE")
            .task_definition(TASK_DEFINITION)
            .build();

        let message = snapshot_from(output_with(service), "c", "worker")
            .unwrap_err()
            .to_string();

        assert!(message.contains("awsvpc"));
    }

    #[test]
    fn keepalive_seconds_match_the_sleep_command() {
        assert_eq!(KEEPALIVE_COMMAND[1], KEEPALIVE_SECONDS.to_string());
    }

    const TASK_ARN: &str = "arn:aws:ecs:us-east-1:123456789012:task/c/0123456789abcdef";

    fn described(task: Task) -> DescribeTasksOutput {
        DescribeTasksOutput::builder().tasks(task).build()
    }

    fn running_task() -> aws_sdk_ecs::types::builders::TaskBuilder {
        Task::builder()
            .task_arn(TASK_ARN)
            .task_definition_arn(TASK_DEFINITION)
            .last_status("RUNNING")
    }

    #[test]
    fn described_task_is_returned_as_is() {
        let task = running_task().build();

        assert_eq!(task_from(described(task.clone())).unwrap(), task);
    }

    #[test]
    fn describe_task_failure_is_an_error_with_reason() {
        let output = DescribeTasksOutput::builder()
            .failures(Failure::builder().reason("MISSING").build())
            .build();

        let message = task_from(output).unwrap_err().to_string();

        assert!(message.contains("MISSING"), "{message}");
    }

    #[test]
    fn own_task_carries_status_task_definition_and_times() {
        let created = DateTime::from_secs(1_700_000_000);
        let started = DateTime::from_secs(1_700_000_030);
        let output = described(
            running_task()
                .created_at(created)
                .started_at(started)
                .build(),
        );

        assert_eq!(
            own_tasks_from(output).unwrap(),
            [OwnTask {
                task_arn: TASK_ARN.into(),
                last_status: "RUNNING".into(),
                task_definition_arn: TASK_DEFINITION.into(),
                created_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
                started_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_030)),
                is_run: false,
            }]
        );
    }

    #[test]
    fn pending_task_has_no_start_time() {
        let output = described(running_task().last_status("PENDING").build());

        assert_eq!(own_tasks_from(output).unwrap()[0].started_at, None);
    }

    #[test]
    fn task_tagged_with_run_mode_is_a_run_task() {
        let tag = |key: &str, value: &str| Tag::builder().key(key).value(value).build();
        let is_run = |tags: Vec<Tag>| {
            let output = described(running_task().set_tags(Some(tags)).build());
            own_tasks_from(output).unwrap()[0].is_run
        };

        assert!(is_run(vec![tag("team", "x"), tag("ecsh:mode", "run")]));
        assert!(!is_run(vec![]));
        assert!(!is_run(vec![tag("ecsh:mode", "exec")]));
        assert!(!is_run(vec![tag("mode", "run")]));
    }

    #[test]
    fn task_that_disappeared_after_listing_is_left_out() {
        let output = DescribeTasksOutput::builder()
            .tasks(running_task().build())
            .failures(
                Failure::builder()
                    .arn("arn:aws:ecs:us-east-1:123456789012:task/c/gone")
                    .reason("MISSING")
                    .build(),
            )
            .build();

        let tasks = own_tasks_from(output).unwrap();

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_arn, TASK_ARN);
    }

    #[test]
    fn other_describe_tasks_failure_is_an_error_with_reason() {
        let output = DescribeTasksOutput::builder()
            .failures(Failure::builder().reason("ACCESS_DENIED").build())
            .build();

        let message = own_tasks_from(output).unwrap_err().to_string();

        assert!(message.contains("ACCESS_DENIED"), "{message}");
    }
}
