use std::time::Instant;

use anyhow::Result;

use super::launch::{self, Prepared};
use crate::agent_wait;
use crate::aws_error;
use crate::config::Profile;
use crate::ecs::{self, Workload};
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
    let Prepared {
        aws_profile,
        client,
        snapshot,
        started_by,
    } = launch::prepare(name, profile, yes, None).await?;
    let explain = |error| aws_error::explain(error, &aws_profile);

    // RunTask の後で登録すると、RunTask の最中のシグナルで ARN を知らないまま終了し、タスクが残る。ここで受けたシグナルは Agent 待ちの入口で拾って止める
    let mut signals = Signals::listen()?;
    let task = ecs::run_task(
        &client,
        &profile.cluster,
        &profile.container,
        &snapshot,
        &started_by,
        Workload::Shell,
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
