use std::env::{self, VarError};
use std::time::Instant;

use anyhow::{Result, bail};
use aws_sdk_ecs::types::{AssignPublicIp, AwsVpcConfiguration};

use crate::agent_wait;
use crate::aws_error;
use crate::aws_profile::AwsProfile;
use crate::config::Profile;
use crate::ecs;
use crate::prompt;
use crate::report::report;
use crate::session::{self, Target};
use crate::session_lock::{self, SessionLock};
use crate::signals::{Signals, Stage};
use crate::ui::{self, Style};

mod stop;

pub use stop::StopAbandoned;

pub async fn exec(name: &str, profile: &Profile, yes: bool) -> Result<()> {
    let plugin = session::find_plugin()?;
    let shell_command = session::shell_command(name);
    let style = Style::current();
    report!();
    report!(
        "  {}  →  {} / {} / {}",
        style.bold(name),
        profile.cluster,
        profile.service,
        profile.container
    );

    let aws_profile = AwsProfile::resolve(profile.aws_profile.as_deref())?;
    report!("  AWS  {aws_profile} · {}", profile.region);
    let started_by = ecs::started_by(&current_user()?)?;

    let explain = |error| aws_error::explain(error, &aws_profile);
    let client = ecs::client(&profile.region, &aws_profile).await;
    let snapshot = ecs::describe_service(&client, &profile.cluster, &profile.service)
        .await
        .map_err(explain)?;
    report!("{}", style.note(network_line(&snapshot.network)));
    report!();
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
    .await
    .map_err(explain)?;
    report!(
        "{}",
        style.success(format!(
            "タスクを起動しました  {}（{}）",
            ui::short_task_id(&task.task_arn),
            ui::task_definition_name(&task.task_definition_arn)
        ))
    );
    report!(
        "{}",
        style.note(format!(
            "12 時間後に自動で止まります · startedBy {started_by}"
        ))
    );
    let _session_lock = hold_session_lock(&task.task_arn);

    let mut entered_at: Option<Instant> = None;
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
            report!("{}", style.note("exit で抜けるとタスクを止めます"));
            report!();
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
        entered_at = Some(Instant::now());
        let status = match signals
            .watch(Stage::InSession, session::wait(&mut child))
            .await
        {
            Ok(status) => status?,
            Err(interruption) => {
                if let Err(error) = session::kill(&mut child).await {
                    ui::report_error(&error);
                }
                return Err(interruption.into());
            }
        };
        if let Some(message) = session::abnormal_exit_message(status) {
            report!("{}", style.warning(message));
        }
        Ok(())
    }
    .await
    .map_err(explain);
    let in_session = entered_at.map(|at| at.elapsed());
    stop::stop_after(
        &client,
        &profile.cluster,
        &task.task_arn,
        used,
        in_session,
        &mut signals,
        &aws_profile,
    )
    .await
}

/// `ネットワーク  subnet-01234567…, subnet-89abcdef… · sg-01234567… · パブリック IP なし`
fn network_line(network: &AwsVpcConfiguration) -> String {
    let ids = |ids: &[String]| {
        ids.iter()
            .map(|id| ui::short_resource_id(id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let public_ip = match network.assign_public_ip() {
        Some(AssignPublicIp::Enabled) => "あり",
        Some(AssignPublicIp::Disabled) => "なし",
        _ => "(未指定)",
    };
    format!(
        "ネットワーク  {} · {} · パブリック IP {public_ip}",
        ids(network.subnets()),
        ids(network.security_groups())
    )
}

/// 取れなくても exec は続ける。困るのは ps / gc で接続中と分からないことだけで、タスクは使える
fn hold_session_lock(task_arn: &str) -> Option<SessionLock> {
    let acquired = session_lock::sessions_dir()
        .and_then(|dir| SessionLock::acquire(&dir, ui::task_id(task_arn)));
    match acquired {
        Ok(lock) => Some(lock),
        Err(error) => {
            report!(
                "{}",
                Style::current().warning(format!(
                    "接続中の印を付けられなかったので、ps / gc でこのタスクを接続中と判定できません: {error:#}"
                ))
            );
            None
        }
    }
}

pub(super) fn current_user() -> Result<String> {
    match env::var("USER") {
        Ok(user) => Ok(user),
        Err(VarError::NotPresent) => bail!("環境変数 USER が設定されていません"),
        Err(VarError::NotUnicode(_)) => bail!("環境変数 USER が UTF-8 ではありません"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_line_shows_shortened_subnets_security_groups_and_public_ip() {
        let network = AwsVpcConfiguration::builder()
            .subnets("subnet-0123456789abcdef0")
            .subnets("subnet-89abcdef012345678")
            .security_groups("sg-0123456789abcdef0")
            .assign_public_ip(AssignPublicIp::Disabled)
            .build()
            .unwrap();

        assert_eq!(
            network_line(&network),
            "ネットワーク  subnet-01234567…, subnet-89abcdef… · sg-01234567… · パブリック IP なし"
        );
    }

    #[test]
    fn network_line_says_unspecified_when_public_ip_is_not_set() {
        let network = AwsVpcConfiguration::builder()
            .subnets("subnet-0123")
            .build()
            .unwrap();

        assert!(network_line(&network).ends_with("パブリック IP (未指定)"));
    }
}
