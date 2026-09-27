use std::time::SystemTime;

use anyhow::{Result, bail};

use super::launch::current_user;
use super::ps::listing::{self, ListedTask, Scope};
use crate::aws_error;
use crate::config::Profile;
use crate::console;
use crate::ecs;
use crate::logs::{self, LogDestination};
use crate::prompt::Terminal;
use crate::report::report;
use crate::session_lock;
use crate::ui::{self, Style};

mod choice;

pub use choice::Page;

/// そのプロファイルで動いている自分のタスクを選び、タスクの詳細かログを AWS コンソールで開く
///
/// `page` を指定したら、開くページを選ばせない
pub async fn open(name: &str, profile: &Profile, page: Option<Page>) -> Result<()> {
    let scope = Scope::resolve(name, profile)?;
    let started_by = ecs::started_by(&current_user()?)?;
    let sessions_dir = session_lock::sessions_dir()?;
    let tasks = listing::list_tasks(&scope, &started_by, &sessions_dir).await?;
    let Some(task) = choice::choose_task(&tasks, &mut Terminal, SystemTime::now())? else {
        report!("動いているタスクはありません");
        return Ok(());
    };
    let url = match choice::decided_page(page, task.task.is_run)? {
        Some(Page::Task) => task_url(profile, task),
        Some(Page::Logs) => match log_destination(&scope, task).await? {
            LogDestination::Awslogs(stream) => console::log_stream_url(&stream),
            LogDestination::Unreadable(reason) => bail!("{reason}ので、ログを開けません"),
        },
        None => match log_destination(&scope, task).await? {
            LogDestination::Awslogs(stream) => match choice::choose_page(&mut Terminal)? {
                Page::Task => task_url(profile, task),
                Page::Logs => console::log_stream_url(&stream),
            },
            LogDestination::Unreadable(reason) => {
                report!(
                    "{}",
                    Style::current().note(format!("{reason}ので、タスクの詳細を開きます"))
                );
                task_url(profile, task)
            }
        },
    };
    console::open_in_browser(&url)
}

fn task_url(profile: &Profile, task: &ListedTask) -> String {
    console::task_url(&profile.region, &profile.cluster, &task.task.task_arn)
}

async fn log_destination(scope: &Scope<'_>, task: &ListedTask) -> Result<LogDestination> {
    let profile = scope.profile;
    let client = ecs::client(&profile.region, &scope.aws_profile).await;
    let definition =
        ecs::container_definition(&client, &task.task.task_definition_arn, &profile.container)
            .await
            .map_err(|error| aws_error::explain(error, &scope.aws_profile))?;
    Ok(logs::destination(
        definition.log_configuration(),
        &profile.container,
        ui::task_id(&task.task.task_arn),
        &profile.region,
    ))
}
