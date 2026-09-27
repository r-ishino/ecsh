//! ps / gc が扱う自分のタスクを集め、接続の状態を付ける

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, bail};
use futures_util::future::join_all;

use crate::aws_error;
use crate::aws_profile::AwsProfile;
use crate::config::{Config, Profile};
use crate::ecs::{self, OwnTask};
use crate::report::report;
use crate::session_lock::{self, Connection};
use crate::ui::{self, Style};

/// タスクを問い合わせる先
#[derive(Debug)]
pub struct Scope<'a> {
    pub name: &'a str,
    pub profile: &'a Profile,
    pub aws_profile: AwsProfile,
}

impl<'a> Scope<'a> {
    pub fn resolve(name: &'a str, profile: &'a Profile) -> Result<Self> {
        Ok(Self {
            name,
            profile,
            aws_profile: AwsProfile::resolve(profile.aws_profile.as_deref())?,
        })
    }

    /// 同じ値なら、問い合わせて返るタスクも同じ
    fn target(&self) -> (String, String, Option<String>) {
        (
            self.profile.region.clone(),
            self.profile.cluster.clone(),
            self.aws_profile.name().map(str::to_owned),
        )
    }
}

/// 設定の全プロファイル。同じ region・cluster・AWS プロファイルを指すものは、設定で先に出てくる 1 つだけ残す
pub fn distinct_scopes(config: &Config) -> Result<Vec<Scope<'_>>> {
    let scopes = config
        .profiles()
        .map(|(name, profile)| Scope::resolve(name, profile))
        .collect::<Result<Vec<_>>>()?;
    Ok(dedupe(scopes))
}

fn dedupe(scopes: Vec<Scope<'_>>) -> Vec<Scope<'_>> {
    let mut seen = HashSet::new();
    scopes
        .into_iter()
        .filter(|scope| seen.insert(scope.target()))
        .collect()
}

/// タスクにこの Mac の ecsh が入っているか。run のタスクは接続しないのが正常なので別に分ける
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connected,
    Abandoned,
    Unknown,
    Run,
}

impl ConnectionState {
    fn of(task: &OwnTask, sessions_dir: &Path) -> Result<Self> {
        if task.is_run {
            return Ok(Self::Run);
        }
        Ok(
            match session_lock::connection(sessions_dir, ui::task_id(&task.task_arn))? {
                Connection::Connected => Self::Connected,
                Connection::Abandoned => Self::Abandoned,
                Connection::Unknown => Self::Unknown,
            },
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "接続中",
            Self::Abandoned => "止め忘れ",
            Self::Unknown => "不明",
            Self::Run => "run",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListedTask {
    pub task: OwnTask,
    pub connection: ConnectionState,
}

/// scope のクラスタで自分が起動したタスクを、起動の古い順に返す。SSO 切れなら `aws sso login` の案内を付ける
pub async fn list_tasks(
    scope: &Scope<'_>,
    started_by: &str,
    sessions_dir: &Path,
) -> Result<Vec<ListedTask>> {
    let client = ecs::client(&scope.profile.region, &scope.aws_profile).await;
    let mut tasks = ecs::list_own_tasks(&client, &scope.profile.cluster, started_by)
        .await
        .map_err(|error| aws_error::explain(error, &scope.aws_profile))?;
    tasks.sort_by_key(|task| task.created_at);
    tasks
        .into_iter()
        .map(|task| {
            Ok(ListedTask {
                connection: ConnectionState::of(&task, sessions_dir)?,
                task,
            })
        })
        .collect()
}

/// 各 scope を並行に問い合わせる。結果は scopes と同じ順
pub async fn list_each<'s, 'a>(
    scopes: &'s [Scope<'a>],
    started_by: &str,
    sessions_dir: &Path,
) -> Vec<(&'s Scope<'a>, Result<Vec<ListedTask>>)> {
    join_all(
        scopes
            .iter()
            .map(|scope| async move { (scope, list_tasks(scope, started_by, sessions_dir).await) }),
    )
    .await
}

/// `list_each` の結果のうち、取れたタスク
pub struct Gathered<'r, 'a> {
    pub tasks: Vec<(&'r Scope<'a>, &'r ListedTask)>,
    /// 全プロファイルのタスクが取れた。欠けていると、ロックファイルの片付けに使えない
    pub complete: bool,
}

/// 取れたタスクを 1 つにまとめる。失敗したプロファイルは警告して飛ばし、全部失敗したらエラーにする
pub fn gather<'r, 'a>(
    results: &'r [(&'r Scope<'a>, Result<Vec<ListedTask>>)],
) -> Result<Gathered<'r, 'a>> {
    let style = Style::current();
    let mut tasks = Vec::new();
    let mut failed = 0;
    for (scope, result) in results {
        match result {
            Ok(listed) => tasks.extend(listed.iter().map(|task| (*scope, task))),
            Err(error) => {
                failed += 1;
                let headline = format!("{} のタスクを取得できないので飛ばします", scope.name);
                for line in style.warning_lines(headline, error) {
                    report!("{line}");
                }
            }
        }
    }
    if failed == results.len() {
        bail!("どのプロファイルのタスクも取得できませんでした");
    }
    Ok(Gathered {
        tasks,
        complete: failed == 0,
    })
}

/// 持ち主のいないロックファイルのうち、`running` に無いタスクのものを消す
///
/// `running` には、ロックファイルを作りうる全クラスタのタスクが揃っていなければならない。欠けたクラスタの止め忘れの印まで消してしまう
pub fn remove_stale_locks(sessions_dir: &Path, running: &HashSet<&str>) -> Result<()> {
    for task_id in session_lock::task_ids(sessions_dir)? {
        if !running.contains(task_id.as_str()) {
            session_lock::remove_if_abandoned(sessions_dir, &task_id)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::session_lock::SessionLock;

    fn profile(region: &str, cluster: &str, aws_profile: Option<&str>) -> Profile {
        Profile {
            region: region.into(),
            cluster: cluster.into(),
            service: "worker".into(),
            container: "app".into(),
            aws_profile: aws_profile.map(str::to_owned),
        }
    }

    fn scope<'a>(name: &'a str, profile: &'a Profile) -> Scope<'a> {
        Scope {
            name,
            profile,
            aws_profile: profile
                .aws_profile
                .clone()
                .map_or(AwsProfile::Default, AwsProfile::Config),
        }
    }

    fn names(scopes: &[Scope<'_>]) -> Vec<String> {
        scopes.iter().map(|scope| scope.name.to_owned()).collect()
    }

    #[test]
    fn profiles_pointing_to_the_same_cluster_are_asked_once_under_the_first_name() {
        let a = profile("us-east-1", "example-staging", Some("example"));
        let b = profile("us-east-1", "example-staging", Some("example"));

        let scopes = dedupe(vec![scope("staging", &a), scope("staging-batch", &b)]);

        assert_eq!(names(&scopes), ["staging"]);
    }

    #[test]
    fn same_cluster_name_in_another_region_or_aws_profile_is_asked_separately() {
        let base = profile("us-east-1", "example", Some("example"));
        let other_region = profile("ap-northeast-1", "example", Some("example"));
        let other_aws_profile = profile("us-east-1", "example", Some("another"));
        let default_aws_profile = profile("us-east-1", "example", None);

        let scopes = dedupe(vec![
            scope("a", &base),
            scope("b", &other_region),
            scope("c", &other_aws_profile),
            scope("d", &default_aws_profile),
        ]);

        assert_eq!(names(&scopes), ["a", "b", "c", "d"]);
    }

    #[test]
    fn profiles_resolved_to_the_same_aws_profile_are_asked_once() {
        let staging = profile("us-east-1", "example", None);
        let batch = profile("us-east-1", "example", Some("from-config"));
        let from_env = |name, profile| Scope {
            name,
            profile,
            aws_profile: AwsProfile::Env("from-env".into()),
        };

        let scopes = dedupe(vec![
            from_env("staging", &staging),
            from_env("batch", &batch),
        ]);

        assert_eq!(names(&scopes), ["staging"]);
    }

    /// テストごとに別の空ディレクトリ。消すのは OS の一時ディレクトリの掃除に任せる
    fn temp_sessions_dir() -> PathBuf {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ecsh-ps-listing-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        let sessions = dir.join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        sessions
    }

    fn leave_abandoned_lock_file(dir: &Path, task_id: &str) -> PathBuf {
        let path = dir.join(format!("{task_id}.lock"));
        File::create(&path).unwrap();
        path
    }

    fn own_task(task_id: &str, is_run: bool) -> OwnTask {
        OwnTask {
            task_arn: format!("arn:aws:ecs:us-east-1:123456789012:task/c/{task_id}"),
            last_status: "RUNNING".into(),
            task_definition_arn: "arn:aws:ecs:us-east-1:123456789012:task-definition/worker:42"
                .into(),
            created_at: None,
            started_at: None,
            is_run,
        }
    }

    #[test]
    fn connection_follows_the_lock_file_of_the_task() {
        let dir = temp_sessions_dir();
        let _held = SessionLock::acquire(&dir, "connected").unwrap();
        leave_abandoned_lock_file(&dir, "abandoned");

        let state = |task_id| ConnectionState::of(&own_task(task_id, false), &dir).unwrap();

        assert_eq!(state("connected"), ConnectionState::Connected);
        assert_eq!(state("abandoned"), ConnectionState::Abandoned);
        assert_eq!(state("elsewhere"), ConnectionState::Unknown);
    }

    #[test]
    fn run_task_is_run_even_with_an_abandoned_lock_file() {
        let dir = temp_sessions_dir();
        leave_abandoned_lock_file(&dir, "batch");

        assert_eq!(
            ConnectionState::of(&own_task("batch", true), &dir).unwrap(),
            ConnectionState::Run
        );
    }

    #[test]
    fn abandoned_lock_files_of_tasks_no_longer_running_are_removed() {
        let dir = temp_sessions_dir();
        let stopped = leave_abandoned_lock_file(&dir, "stopped");
        let running = leave_abandoned_lock_file(&dir, "running");
        let _held = SessionLock::acquire(&dir, "just-launched").unwrap();

        remove_stale_locks(&dir, &HashSet::from(["running"])).unwrap();

        assert!(!stopped.exists());
        assert!(running.exists());
        assert!(dir.join("just-launched.lock").exists());
    }
}
