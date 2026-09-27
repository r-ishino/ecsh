use std::env::{self, VarError};

use anyhow::{Result, bail};

use crate::agent_wait;
use crate::aws_profile::AwsProfile;
use crate::config::Profile;
use crate::ecs;
use crate::prompt;
use crate::report::report;
use crate::session::{self, Target};
use crate::signals::{Signals, Stage};

mod stop;

pub use stop::StopAbandoned;

pub async fn run(name: &str, profile: &Profile, yes: bool) -> Result<()> {
    let plugin = session::find_plugin()?;
    let shell_command = session::shell_command(name);
    report!(
        "対象: region={} cluster={} service={} container={}",
        profile.region,
        profile.cluster,
        profile.service,
        profile.container
    );

    let aws_profile = AwsProfile::resolve(profile.aws_profile.as_deref())?;
    report!("AWS プロファイル: {aws_profile}");
    let started_by = ecs::started_by(&current_user()?)?;

    let client = ecs::client(&profile.region, &aws_profile).await;
    let snapshot = ecs::describe_service(&client, &profile.cluster, &profile.service).await?;
    let network = &snapshot.network;
    report!("サブネット: {}", network.subnets().join(", "));
    report!(
        "セキュリティグループ: {}",
        network.security_groups().join(", ")
    );
    report!(
        "パブリック IP の割り当て: {}",
        network
            .assign_public_ip()
            .map_or("(未指定)", |a| a.as_str())
    );
    prompt::confirm_launch(name, profile, yes)?;

    // RunTask の後で登録すると、RunTask の最中のシグナルで ARN を知らないまま終了し、タスクが残る。ここで受けたシグナルは Agent 待ちの入口で拾って止める
    let mut signals = Signals::listen()?;
    let task = ecs::run_task(
        &client,
        &profile.cluster,
        &profile.container,
        &snapshot,
        &started_by,
    )
    .await?;
    report!("タスクを起動しました: {}", task.task_arn);
    report!("タスク定義: {}", task.task_definition_arn);
    report!("startedBy: {started_by}");
    report!("12 時間後に自動で止まります");

    // ここから先の `?` は async ブロックを抜けるだけで、どのエラーでも下の stop_after がタスクを止める
    let used: Result<()> = async {
        let enter = async {
            let runtime_id = agent_wait::wait_until_exec_ready(
                &client,
                &profile.cluster,
                &task.task_arn,
                &profile.container,
            )
            .await?;
            let target = Target {
                region: &profile.region,
                cluster: &profile.cluster,
                task_arn: &task.task_arn,
                container: &profile.container,
                runtime_id: &runtime_id,
            };
            session::start(
                &client,
                &plugin,
                &target,
                &shell_command,
                aws_profile.name(),
            )
            .await
        };
        let mut child = signals.watch(Stage::Preparing, enter).await??;
        let status = match signals
            .watch(Stage::InSession, session::wait(&mut child))
            .await
        {
            Ok(status) => status?,
            Err(interruption) => {
                if let Err(error) = session::kill(&mut child).await {
                    report!("{error:#}");
                }
                return Err(interruption.into());
            }
        };
        if let Some(message) = session::abnormal_exit_message(status) {
            report!("{message}");
        }
        Ok(())
    }
    .await;
    stop::stop_after(
        &client,
        &profile.cluster,
        &task.task_arn,
        used,
        &mut signals,
    )
    .await
}

fn current_user() -> Result<String> {
    match env::var("USER") {
        Ok(user) => Ok(user),
        Err(VarError::NotPresent) => bail!("環境変数 USER が設定されていません"),
        Err(VarError::NotUnicode(_)) => bail!("環境変数 USER が UTF-8 ではありません"),
    }
}
