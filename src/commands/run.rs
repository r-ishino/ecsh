use anyhow::{Result, bail};

use crate::config::Profile;
use crate::ecs;

pub async fn run(profile: &Profile) -> Result<()> {
    eprintln!(
        "対象: region={} cluster={} service={} container={}",
        profile.region, profile.cluster, profile.service, profile.container
    );

    let client = ecs::client(&profile.region).await;
    let snapshot = ecs::describe_service(&client, &profile.cluster, &profile.service).await?;
    let network = &snapshot.network;
    eprintln!("タスク定義: {}", snapshot.task_definition);
    eprintln!("サブネット: {}", network.subnets().join(", "));
    eprintln!(
        "セキュリティグループ: {}",
        network.security_groups().join(", ")
    );
    eprintln!(
        "パブリック IP の割り当て: {}",
        network
            .assign_public_ip()
            .map_or("(未指定)", |a| a.as_str())
    );

    bail!("RunTask 以降は未実装です")
}
