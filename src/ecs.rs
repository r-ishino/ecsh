use anyhow::{Context, Result, bail};
use aws_config::{BehaviorVersion, Region};
use aws_sdk_ecs::Client;
use aws_sdk_ecs::operation::describe_services::DescribeServicesOutput;
use aws_sdk_ecs::operation::run_task::RunTaskOutput;
use aws_sdk_ecs::operation::run_task::builders::RunTaskFluentBuilder;
use aws_sdk_ecs::operation::stop_task::builders::StopTaskFluentBuilder;
use aws_sdk_ecs::types::{
    AwsVpcConfiguration, CapacityProviderStrategyItem, ContainerOverride, LaunchType,
    NetworkConfiguration, TaskOverride,
};

use crate::aws_profile::AwsProfile;

pub async fn client(region: &str, aws_profile: &AwsProfile) -> Client {
    let mut loader =
        aws_config::defaults(BehaviorVersion::latest()).region(Region::new(region.to_owned()));
    if let Some(name) = aws_profile.name() {
        loader = loader.profile_name(name);
    }
    Client::new(&loader.load().await)
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

pub async fn run_task(
    client: &Client,
    cluster: &str,
    container: &str,
    snapshot: &ServiceSnapshot,
    started_by: &str,
) -> Result<LaunchedTask> {
    let output = run_task_request(client, cluster, container, snapshot, started_by)
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
) -> RunTaskFluentBuilder {
    let keepalive = ContainerOverride::builder()
        .name(container)
        .set_command(Some(KEEPALIVE_COMMAND.map(String::from).to_vec()))
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
        .enable_execute_command(true)
        .started_by(started_by)
        .overrides(
            TaskOverride::builder()
                .container_overrides(keepalive)
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

/// StopTask の reason。コンソールのタスクの停止理由に出る
pub const STOP_REASON: &str = "Stopped by ecsh";

pub async fn stop_task(client: &Client, cluster: &str, task_arn: &str) -> Result<()> {
    stop_task_request(client, cluster, task_arn)
        .send()
        .await
        .with_context(|| format!("StopTask に失敗しました（cluster={cluster}）"))?;
    Ok(())
}

fn stop_task_request(client: &Client, cluster: &str, task_arn: &str) -> StopTaskFluentBuilder {
    client
        .stop_task()
        .cluster(cluster)
        .task(task_arn)
        .reason(STOP_REASON)
}

#[cfg(test)]
mod tests {
    use aws_sdk_ecs::types::{AssignPublicIp, Failure, Service, Task};

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
        let request = run_task_request(&offline_client(), "c", "app", &snapshot(), "ecsh/me");
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
        let request = run_task_request(&offline_client(), "c", "app", &snapshot(), "ecsh/me");
        let input = request.as_input();

        assert_eq!(input.get_enable_execute_command(), &Some(true));
        assert_eq!(input.get_started_by().as_deref(), Some("ecsh/me"));
    }

    #[test]
    fn run_task_request_replaces_container_command_with_12_hour_sleep() {
        let request = run_task_request(&offline_client(), "c", "app", &snapshot(), "ecsh/me");
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
        let request = stop_task_request(&offline_client(), "c", task_arn);
        let input = request.as_input();

        assert_eq!(input.get_cluster().as_deref(), Some("c"));
        assert_eq!(input.get_task().as_deref(), Some(task_arn));
        assert_eq!(input.get_reason().as_deref(), Some("Stopped by ecsh"));
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
}
