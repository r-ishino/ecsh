use anyhow::{Context, Result, bail};
use aws_config::{BehaviorVersion, Region};
use aws_sdk_ecs::Client;
use aws_sdk_ecs::operation::describe_services::DescribeServicesOutput;
use aws_sdk_ecs::types::AwsVpcConfiguration;

pub async fn client(region: &str) -> Client {
    let sdk_config = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(region.to_owned()))
        .load()
        .await;
    Client::new(&sdk_config)
}

/// 使い捨てタスクを起動するときにサービスから写す設定
#[derive(Debug, PartialEq)]
pub struct ServiceSnapshot {
    /// サービスに今デプロイされているタスク定義の ARN
    pub task_definition: String,
    pub network: AwsVpcConfiguration,
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
        task_definition: task_definition.to_owned(),
        network: network.clone(),
    })
}

#[cfg(test)]
mod tests {
    use aws_sdk_ecs::types::{AssignPublicIp, Failure, NetworkConfiguration, Service};

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
            .network_configuration(
                NetworkConfiguration::builder()
                    .awsvpc_configuration(awsvpc())
                    .build(),
            )
    }

    fn output_with(service: Service) -> DescribeServicesOutput {
        DescribeServicesOutput::builder().services(service).build()
    }

    #[test]
    fn active_service_yields_task_definition_and_awsvpc_config() {
        let snapshot = snapshot_from(output_with(active_service().build()), "c", "worker").unwrap();

        assert_eq!(
            snapshot,
            ServiceSnapshot {
                task_definition: TASK_DEFINITION.into(),
                network: awsvpc(),
            }
        );
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
