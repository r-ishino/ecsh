use std::env::{self, VarError};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::aws_profile::AwsProfile;
use crate::config::Profile;
use crate::ecs;

mod stop;

pub async fn run(profile: &Profile) -> Result<()> {
    eprintln!(
        "対象: region={} cluster={} service={} container={}",
        profile.region, profile.cluster, profile.service, profile.container
    );

    let aws_profile = AwsProfile::resolve(profile.aws_profile.as_deref())?;
    eprintln!("AWS プロファイル: {aws_profile}");
    let started_by = ecs::started_by(&current_user()?)?;

    let client = ecs::client(&profile.region, &aws_profile).await;
    let snapshot = ecs::describe_service(&client, &profile.cluster, &profile.service).await?;
    let network = &snapshot.network;
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

    let task = ecs::run_task(
        &client,
        &profile.cluster,
        &profile.container,
        &snapshot,
        &started_by,
    )
    .await?;
    eprintln!("タスクを起動しました: {}", task.task_arn);
    eprintln!("タスク定義: {}", task.task_definition_arn);
    eprintln!("startedBy: {started_by}");
    eprintln!("12 時間後に自動で止まります");

    // ここから先の `?` は async ブロックを抜けるだけで、どのエラーでも下の stop_after がタスクを止める
    let used: Result<()> = async {
        eprintln!("exec は未実装のため、5 秒後に止めます");
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(())
    }
    .await;
    stop::stop_after(&client, &profile.cluster, &task.task_arn, used).await
}

fn current_user() -> Result<String> {
    match env::var("USER") {
        Ok(user) => Ok(user),
        Err(VarError::NotPresent) => bail!("環境変数 USER が設定されていません"),
        Err(VarError::NotUnicode(_)) => bail!("環境変数 USER が UTF-8 ではありません"),
    }
}
