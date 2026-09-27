use std::collections::HashSet;
use std::io::{self, Write};
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::config::{Config, Profile};
use crate::ecs;
use crate::report::report;
use crate::session_lock;
use crate::ui::{self, Style};

use super::exec::current_user;

pub(super) mod listing;
mod table;

use listing::{ConnectionState, Scope};
use table::Row;

/// そのプロファイルのクラスタで、自分が ecsh で起動して動いているタスクを一覧する
pub async fn ps(name: &str, profile: &Profile) -> Result<()> {
    let scope = Scope::resolve(name, profile)?;
    let started_by = ecs::started_by(&current_user()?)?;
    let sessions_dir = session_lock::sessions_dir()?;
    let tasks = listing::list_tasks(&scope, &started_by, &sessions_dir).await?;
    let rows: Vec<Row<'_>> = tasks
        .iter()
        .map(|task| Row {
            profile: name,
            task,
        })
        .collect();
    // ほかのクラスタのタスクは一覧に無いので、ロックファイルは消さない。消すと別のクラスタの止め忘れの印まで消える
    show(&rows, false, &format!("ecsh gc {name}"))
}

/// 設定の全プロファイルを並行に回り、1 つの表にまとめる。失敗したプロファイルは警告して飛ばす
pub async fn ps_all(config: &Config) -> Result<()> {
    let scopes = listing::distinct_scopes(config)?;
    let started_by = ecs::started_by(&current_user()?)?;
    let sessions_dir = session_lock::sessions_dir()?;
    let results = listing::list_each(&scopes, &started_by, &sessions_dir).await;
    let gathered = listing::gather(&results)?;
    let rows: Vec<Row<'_>> = gathered
        .tasks
        .iter()
        .map(|(scope, task)| Row {
            profile: scope.name,
            task,
        })
        .collect();
    show(&rows, true, "ecsh gc --all")?;
    if gathered.complete {
        remove_stale_locks(&sessions_dir, &rows);
    }
    Ok(())
}

fn show(rows: &[Row<'_>], with_profile: bool, gc_command: &str) -> Result<()> {
    if rows.is_empty() {
        report!("動いているタスクはありません");
        return Ok(());
    }
    let lines = table::render(rows, with_profile, SystemTime::now(), Style::for_stdout());
    print_lines(&lines)?;
    if let Some(hint) = gc_hint(rows, gc_command) {
        report!("{}", Style::current().warning(hint));
    }
    Ok(())
}

fn gc_hint(rows: &[Row<'_>], gc_command: &str) -> Option<String> {
    rows.iter()
        .any(|row| row.task.connection == ConnectionState::Abandoned)
        .then(|| format!("止め忘れのタスクは {gc_command} で止められます"))
}

/// `ecsh ps | head -1` のように読み手が先に閉じたら、残りを捨てて正常に終える
fn print_lines(lines: &[String]) -> Result<()> {
    let mut stdout = io::stdout().lock();
    let written = lines
        .iter()
        .try_for_each(|line| writeln!(stdout, "{line}"))
        .and_then(|()| stdout.flush());
    match written {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        written => written.context("一覧を標準出力に書けません"),
    }
}

/// 一覧は済んでいるので、片付けに失敗しても警告だけにする
fn remove_stale_locks(sessions_dir: &Path, rows: &[Row<'_>]) {
    let running: HashSet<&str> = rows
        .iter()
        .map(|row| ui::task_id(&row.task.task.task_arn))
        .collect();
    if let Err(error) = listing::remove_stale_locks(sessions_dir, &running) {
        for line in Style::current().warning_lines("止め忘れの印を片付けられませんでした", &error)
        {
            report!("{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::listing::ListedTask;
    use super::*;
    use crate::ecs::OwnTask;

    fn listed(connection: ConnectionState) -> ListedTask {
        ListedTask {
            task: OwnTask {
                task_arn: "arn:aws:ecs:us-east-1:123456789012:task/c/0123456789abcdef".into(),
                last_status: "RUNNING".into(),
                task_definition_arn: "arn:aws:ecs:us-east-1:123456789012:task-definition/worker:42"
                    .into(),
                created_at: None,
                started_at: None,
                is_run: connection == ConnectionState::Run,
            },
            connection,
        }
    }

    fn hint_for(connections: &[ConnectionState], gc_command: &str) -> Option<String> {
        let tasks: Vec<ListedTask> = connections.iter().copied().map(listed).collect();
        let rows: Vec<Row<'_>> = tasks
            .iter()
            .map(|task| Row {
                profile: "staging",
                task,
            })
            .collect();
        gc_hint(&rows, gc_command)
    }

    #[test]
    fn abandoned_task_points_to_gc_for_the_listed_profile() {
        assert_eq!(
            hint_for(
                &[ConnectionState::Connected, ConnectionState::Abandoned],
                "ecsh gc staging"
            )
            .as_deref(),
            Some("止め忘れのタスクは ecsh gc staging で止められます")
        );
        assert_eq!(
            hint_for(&[ConnectionState::Abandoned], "ecsh gc --all").as_deref(),
            Some("止め忘れのタスクは ecsh gc --all で止められます")
        );
    }

    #[test]
    fn gc_is_not_suggested_without_abandoned_tasks() {
        let others = [
            ConnectionState::Connected,
            ConnectionState::Unknown,
            ConnectionState::Run,
        ];

        assert_eq!(hint_for(&others, "ecsh gc staging"), None);
    }
}
