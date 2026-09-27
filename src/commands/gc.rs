use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal};
use std::path::Path;
use std::time::SystemTime;

use anyhow::{Result, bail};
use aws_sdk_ecs::Client;

use crate::aws_error;
use crate::config::{Config, Profile};
use crate::ecs;
use crate::prompt::{Abort, Console, Terminal};
use crate::report::report;
use crate::session_lock;
use crate::ui::{self, Style};

use super::exec::current_user;
use super::ps::listing::{self, ConnectionState, ListedTask, Scope};

/// そのプロファイルのクラスタで、自分が ecsh で起動して残ったタスクを選んで止める
pub async fn gc(name: &str, profile: &Profile, yes: bool) -> Result<()> {
    ensure_can_choose(yes, io::stdin().is_terminal())?;
    let scope = Scope::resolve(name, profile)?;
    let started_by = ecs::started_by(&current_user()?)?;
    let sessions_dir = session_lock::sessions_dir()?;
    let tasks = listing::list_tasks(&scope, &started_by, &sessions_dir).await?;
    let listed: Vec<Listed<'_, '_>> = tasks.iter().map(|task| (&scope, task)).collect();
    // ほかのクラスタのタスクは一覧に無いので、止めたもの以外のロックファイルは消さない（ps と同じ）
    sweep(&listed, false, yes, &sessions_dir).await
}

/// 設定の全プロファイルを回り、1 つの一覧にまとめて止める。失敗したプロファイルは警告して飛ばす
pub async fn gc_all(config: &Config, yes: bool) -> Result<()> {
    ensure_can_choose(yes, io::stdin().is_terminal())?;
    let scopes = listing::distinct_scopes(config)?;
    let started_by = ecs::started_by(&current_user()?)?;
    let sessions_dir = session_lock::sessions_dir()?;
    let results = listing::list_each(&scopes, &started_by, &sessions_dir).await;
    let gathered = listing::gather(&results)?;
    if gathered.complete {
        let running: HashSet<&str> = gathered
            .tasks
            .iter()
            .map(|(_, task)| ui::task_id(&task.task.task_arn))
            .collect();
        warn_on_error(
            "止め忘れの印を片付けられませんでした",
            listing::remove_stale_locks(&sessions_dir, &running),
        );
    }
    sweep(&gathered.tasks, true, yes, &sessions_dir).await
}

type Listed<'r, 'a> = (&'r Scope<'a>, &'r ListedTask);

/// 一覧に出さない接続中を除いて選ばせ、選んだものを 1 件ずつ止める
async fn sweep(
    listed: &[Listed<'_, '_>],
    with_profile: bool,
    yes: bool,
    sessions_dir: &Path,
) -> Result<()> {
    let style = Style::current();
    let (candidates, connected) = candidates(listed);
    if connected > 0 {
        report!("{}", style.note(format!("接続中 {connected} 件は対象外")));
    }
    let checked = checked_by_default(&candidates);
    let chosen = if yes {
        let runs = candidates.len() - checked.len();
        if runs > 0 {
            report!(
                "{}",
                style.note(format!("run のタスク {runs} 件は --yes では止めません"))
            );
        }
        checked
    } else if candidates.is_empty() {
        Vec::new()
    } else {
        let items = labels(&candidates, with_profile, SystemTime::now(), style);
        chosen(Terminal.multi_select("止めるタスクを選んでください", items, &checked)?)?
    };
    if chosen.is_empty() {
        report!("止めるタスクはありません");
        return Ok(());
    }

    let mut clients: HashMap<&str, Client> = HashMap::new();
    let mut failed = 0;
    for &index in &chosen {
        let (scope, task) = candidates[index];
        if !clients.contains_key(scope.name) {
            let client = ecs::client(&scope.profile.region, &scope.aws_profile).await;
            clients.insert(scope.name, client);
        }
        let client = &clients[scope.name];
        let task_arn = &task.task.task_arn;
        match ecs::stop_task(
            client,
            &scope.profile.cluster,
            task_arn,
            ecs::GC_STOP_REASON,
        )
        .await
        {
            Ok(()) => {
                report!(
                    "{}",
                    style.success(stopped_message(scope.name, task, with_profile, style))
                );
                warn_on_error(
                    "止めたタスクの印を消せませんでした",
                    session_lock::remove_if_abandoned(sessions_dir, ui::task_id(task_arn)),
                );
            }
            Err(error) => {
                failed += 1;
                let error = aws_error::explain(error, &scope.aws_profile)
                    .context(format!("タスクを止められませんでした: {task_arn}"));
                for line in style.error_lines(&error) {
                    report!("{line}");
                }
            }
        }
    }
    conclude(failed)
}

/// 一覧で選ぶときも、stdin がターミナルでなければ選べない。--yes なら選ばずに止める
fn ensure_can_choose(yes: bool, stdin_is_terminal: bool) -> Result<()> {
    if !yes && !stdin_is_terminal {
        bail!(
            "止めるタスクを一覧から選びますが、標準入力がターミナルではありません。一覧を出さずに止め忘れと不明のタスクを止めるには --yes を付けてください"
        );
    }
    Ok(())
}

/// 一覧に出すタスクと、出さない接続中の件数
fn candidates<'r, 'a>(listed: &[Listed<'r, 'a>]) -> (Vec<Listed<'r, 'a>>, usize) {
    let (connected, candidates): (Vec<_>, Vec<_>) = listed
        .iter()
        .copied()
        .partition(|(_, task)| task.connection == ConnectionState::Connected);
    (candidates, connected.len())
}

/// 最初からチェックを入れておく（--yes なら止める）タスクの添字。run を止めるのは片付けではなく中止なので入れない
fn checked_by_default(candidates: &[Listed<'_, '_>]) -> Vec<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(_, (_, task))| {
            matches!(
                task.connection,
                ConnectionState::Abandoned | ConnectionState::Unknown
            )
        })
        .map(|(index, _)| index)
        .collect()
}

/// 何も選ばずに Enter したときも、Esc と同じく取りやめる
fn chosen(selection: Option<Vec<usize>>) -> Result<Vec<usize>> {
    match selection {
        Some(chosen) if !chosen.is_empty() => Ok(chosen),
        _ => Err(Abort::Cancelled("停止を取りやめました").into()),
    }
}

/// 一部を止められなくても残りは止め、最後に失敗として終える
fn conclude(failed: usize) -> Result<()> {
    if failed > 0 {
        bail!("{failed} 件のタスクを止められませんでした");
    }
    Ok(())
}

/// 一覧の項目。ID・接続・起動から・タスク定義の列を全角込みで揃え、with_profile なら先頭にプロファイル名を太字で出す
fn labels(
    candidates: &[Listed<'_, '_>],
    with_profile: bool,
    now: SystemTime,
    style: Style,
) -> Vec<String> {
    let rows: Vec<[String; 5]> = candidates
        .iter()
        .map(|(scope, task)| {
            [
                scope.name.to_owned(),
                ui::short_task_id(&task.task.task_arn).to_owned(),
                task.connection.label().to_owned(),
                task.task.created_at.map_or("─".to_owned(), |at| {
                    ui::duration(now.duration_since(at).unwrap_or_default())
                }),
                ui::task_definition_name(&task.task.task_definition_arn).to_owned(),
            ]
        })
        .collect();
    let first = usize::from(!with_profile);
    let mut widths = [0; 5];
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(ui::display_width(cell));
        }
    }
    rows.iter()
        .map(|row| {
            (first..row.len())
                .map(|column| {
                    let cell = &row[column];
                    let painted = if column == 0 {
                        style.bold(cell)
                    } else {
                        cell.clone()
                    };
                    if column == row.len() - 1 {
                        painted
                    } else {
                        let padding = widths[column] - ui::display_width(cell);
                        format!("{painted}{}", " ".repeat(padding))
                    }
                })
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect()
}

/// `タスクを止めました  01234567（worker:42）`。with_profile なら ID の前にプロファイル名を太字で出す
fn stopped_message(profile: &str, task: &ListedTask, with_profile: bool, style: Style) -> String {
    let id = ui::short_task_id(&task.task.task_arn);
    let definition = ui::task_definition_name(&task.task.task_definition_arn);
    if with_profile {
        format!(
            "タスクを止めました  {}  {id}（{definition}）",
            style.bold(profile)
        )
    } else {
        format!("タスクを止めました  {id}（{definition}）")
    }
}

/// 止めることは済んでいるので、印の片付けに失敗しても警告だけにする
fn warn_on_error<T>(headline: &str, result: Result<T>) {
    if let Err(error) = result {
        for line in Style::current().warning_lines(headline, &error) {
            report!("{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use anyhow::anyhow;

    use super::*;
    use crate::aws_profile::AwsProfile;
    use crate::ecs::OwnTask;
    use crate::ui::Guidance;

    const PLAIN: Style = Style::PLAIN;

    fn profile() -> Profile {
        Profile {
            region: "us-east-1".into(),
            cluster: "example-staging".into(),
            service: "worker".into(),
            container: "app".into(),
            aws_profile: None,
        }
    }

    fn scope<'a>(name: &'a str, profile: &'a Profile) -> Scope<'a> {
        Scope {
            name,
            profile,
            aws_profile: AwsProfile::Default,
        }
    }

    fn listed(task_id: &str, connection: ConnectionState) -> ListedTask {
        ListedTask {
            task: OwnTask {
                task_arn: format!("arn:aws:ecs:us-east-1:123456789012:task/c/{task_id}"),
                last_status: "RUNNING".into(),
                task_definition_arn: "arn:aws:ecs:us-east-1:123456789012:task-definition/worker:42"
                    .into(),
                created_at: Some(SystemTime::UNIX_EPOCH),
                started_at: None,
                is_run: connection == ConnectionState::Run,
            },
            connection,
        }
    }

    fn ids(candidates: &[Listed<'_, '_>], indices: &[usize]) -> Vec<String> {
        indices
            .iter()
            .map(|&index| ui::short_task_id(&candidates[index].1.task.task_arn).to_owned())
            .collect()
    }

    #[test]
    fn connected_tasks_are_left_out_of_the_list_and_counted() {
        let profile = profile();
        let scope = scope("staging", &profile);
        let tasks = [
            listed("aaaaaaaa", ConnectionState::Connected),
            listed("bbbbbbbb", ConnectionState::Abandoned),
            listed("cccccccc", ConnectionState::Connected),
            listed("dddddddd", ConnectionState::Run),
        ];
        let listed: Vec<Listed<'_, '_>> = tasks.iter().map(|task| (&scope, task)).collect();

        let (candidates, connected) = candidates(&listed);

        assert_eq!(connected, 2);
        assert_eq!(ids(&candidates, &[0, 1]), ["bbbbbbbb", "dddddddd"]);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn abandoned_and_unknown_are_checked_by_default_and_stopped_by_yes_but_run_is_not() {
        let profile = profile();
        let scope = scope("staging", &profile);
        let tasks = [
            listed("aaaaaaaa", ConnectionState::Run),
            listed("bbbbbbbb", ConnectionState::Abandoned),
            listed("cccccccc", ConnectionState::Unknown),
        ];
        let candidates: Vec<Listed<'_, '_>> = tasks.iter().map(|task| (&scope, task)).collect();

        let checked = checked_by_default(&candidates);

        assert_eq!(ids(&candidates, &checked), ["bbbbbbbb", "cccccccc"]);
    }

    #[test]
    fn choosing_is_required_without_yes_only_off_a_terminal() {
        assert!(ensure_can_choose(false, true).is_ok());
        assert!(ensure_can_choose(true, true).is_ok());
        assert!(ensure_can_choose(true, false).is_ok());

        let message = ensure_can_choose(false, false).unwrap_err().to_string();
        assert!(message.contains("標準入力がターミナルではありません"));
        assert!(message.contains("--yes"));
    }

    #[test]
    fn escaping_or_choosing_nothing_cancels_with_exit_code_1() {
        for selection in [None, Some(Vec::new())] {
            let abort = chosen(selection.clone())
                .unwrap_err()
                .downcast::<Abort>()
                .unwrap();

            assert_eq!(
                abort,
                Abort::Cancelled("停止を取りやめました"),
                "{selection:?}"
            );
            assert_eq!(abort.exit_code(), 1);
        }
    }

    #[test]
    fn chosen_tasks_are_stopped() {
        assert_eq!(chosen(Some(vec![0, 2])).unwrap(), [0, 2]);
    }

    #[test]
    fn any_failure_to_stop_ends_as_a_failure_counting_the_tasks_left() {
        assert!(conclude(0).is_ok());
        assert_eq!(
            conclude(2).unwrap_err().to_string(),
            "2 件のタスクを止められませんでした"
        );
    }

    #[test]
    fn failure_to_stop_names_the_full_task_arn_and_keeps_the_sso_login_remedy() {
        let expired = Guidance::new(
            "AWS の SSO セッションが切れています",
            "`aws sso login` を実行してから、もう一度実行してください",
        )
        .wrap(anyhow!("StopTask に失敗しました（cluster=c）"));
        let task_arn = "arn:aws:ecs:us-east-1:123456789012:task/c/0123456789abcdef";

        let error = expired.context(format!("タスクを止められませんでした: {task_arn}"));

        assert_eq!(
            PLAIN.error_lines(&error),
            [
                format!("✗ タスクを止められませんでした: {task_arn}"),
                "  `aws sso login` を実行してから、もう一度実行してください".into(),
                "  AWS の SSO セッションが切れています".into(),
                "  StopTask に失敗しました（cluster=c）".into(),
            ]
        );
    }

    #[test]
    fn list_items_show_id_connection_elapsed_time_and_task_definition_aligned() {
        let profile = profile();
        let scope = scope("staging", &profile);
        let tasks = [
            listed("0123456789abcdef", ConnectionState::Abandoned),
            listed("fedcba9876543210", ConnectionState::Unknown),
        ];
        let candidates: Vec<Listed<'_, '_>> = tasks.iter().map(|task| (&scope, task)).collect();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(125 * 60);

        assert_eq!(
            labels(&candidates, false, now, PLAIN),
            [
                "01234567  止め忘れ  2 時間 5 分  worker:42",
                "fedcba98  不明      2 時間 5 分  worker:42",
            ]
        );
    }

    #[test]
    fn list_items_across_profiles_start_with_the_profile_name_in_bold() {
        let profile = profile();
        let staging = scope("staging", &profile);
        let batch = scope("batch", &profile);
        let tasks = [
            listed("0123456789abcdef", ConnectionState::Abandoned),
            listed("fedcba9876543210", ConnectionState::Run),
        ];
        let candidates = [(&staging, &tasks[0]), (&batch, &tasks[1])];
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(45);

        assert_eq!(
            labels(&candidates, true, now, PLAIN),
            [
                "staging  01234567  止め忘れ  45 秒  worker:42",
                "batch    fedcba98  run       45 秒  worker:42",
            ]
        );
        assert!(
            labels(&candidates, true, now, Style::COLOR)[0].starts_with("\x1b[1mstaging\x1b[0m")
        );
    }

    #[test]
    fn stopped_message_shows_short_id_and_task_definition() {
        let task = listed("0123456789abcdef", ConnectionState::Abandoned);

        assert_eq!(
            stopped_message("staging", &task, false, PLAIN),
            "タスクを止めました  01234567（worker:42）"
        );
        assert_eq!(
            stopped_message("staging", &task, true, PLAIN),
            "タスクを止めました  staging  01234567（worker:42）"
        );
    }
}
