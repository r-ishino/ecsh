use std::time::SystemTime;

use anyhow::{Result, bail};

use super::super::ps::listing::ListedTask;
use crate::prompt::{Abort, Console};
use crate::ui;

/// AWS コンソールで開くページ
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// ECS のタスクの詳細
    Task,
    /// CloudWatch Logs のログストリーム
    Logs,
}

impl Page {
    /// `--task` / `--logs` から。どちらも無ければ None
    pub fn from_flags(task: bool, logs: bool) -> Option<Self> {
        match (task, logs) {
            (true, _) => Some(Self::Task),
            (_, true) => Some(Self::Logs),
            _ => None,
        }
    }
}

const PAGE_LABELS: [(Page, &str); 2] = [
    (Page::Task, "タスクの詳細"),
    (Page::Logs, "ログ（CloudWatch Logs）"),
];

/// 開くタスクを決める。1 件なら選ばせない。候補が無ければ None
pub fn choose_task<'a>(
    tasks: &'a [ListedTask],
    console: &mut impl Console,
    now: SystemTime,
) -> Result<Option<&'a ListedTask>> {
    match tasks {
        [] => return Ok(None),
        [only] => return Ok(Some(only)),
        _ => {}
    }
    if !console.stdin_is_terminal() {
        bail!("標準入力がターミナルではないため、タスクを一覧から選べません");
    }
    match console.select("開くタスクを選んでください", labels(tasks, now))? {
        Some(index) => Ok(Some(&tasks[index])),
        None => Err(Abort::Cancelled("タスクの選択を取りやめました").into()),
    }
}

/// 選ばせずに決まるページ。選ばせるなら None
///
/// exec のタスクのコンテナは sleep しているだけでログが空なので、タスクの詳細に決める
pub fn decided_page(requested: Option<Page>, is_run: bool) -> Result<Option<Page>> {
    match (requested, is_run) {
        (Some(Page::Logs), false) => bail!(
            "exec のタスクはログに何も出ないので、ログは開けません。タスクの詳細なら --task で開けます"
        ),
        (Some(page), _) => Ok(Some(page)),
        (None, false) => Ok(Some(Page::Task)),
        (None, true) => Ok(None),
    }
}

/// run のタスクで、タスクの詳細とログのどちらを開くか選ばせる
pub fn choose_page(console: &mut impl Console) -> Result<Page> {
    if !console.stdin_is_terminal() {
        bail!(
            "標準入力がターミナルではないため、開くページを選べません。--task か --logs を付けてください"
        );
    }
    let items = PAGE_LABELS.map(|(_, label)| label.to_owned()).to_vec();
    match console.select("開くページを選んでください", items)? {
        Some(index) => Ok(PAGE_LABELS[index].0),
        None => Err(Abort::Cancelled("開くページの選択を取りやめました").into()),
    }
}

/// `01234567  run     RUNNING  12 分  worker:42`（ID・接続・状態・起動から・タスク定義）
fn labels(tasks: &[ListedTask], now: SystemTime) -> Vec<String> {
    let columns: Vec<[String; 4]> = tasks
        .iter()
        .map(|listed| {
            let task = &listed.task;
            [
                ui::short_task_id(&task.task_arn).to_owned(),
                listed.connection.label().to_owned(),
                task.last_status.clone(),
                task.created_at
                    .and_then(|created| now.duration_since(created).ok())
                    .map_or("─".to_owned(), ui::duration),
            ]
        })
        .collect();
    let width = |column: usize| {
        columns
            .iter()
            .map(|row| ui::display_width(&row[column]))
            .max()
            .unwrap_or(0)
    };
    let widths = [width(0), width(1), width(2), width(3)];
    columns
        .iter()
        .zip(tasks)
        .map(|(row, listed)| {
            let cells: Vec<String> = row
                .iter()
                .zip(widths)
                .map(|(cell, width)| pad(cell, width))
                .collect();
            format!(
                "{}  {}",
                cells.join("  "),
                ui::task_definition_name(&listed.task.task_definition_arn)
            )
        })
        .collect()
}

fn pad(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(ui::display_width(text));
    format!("{text}{}", " ".repeat(fill))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::commands::ps::listing::ConnectionState;
    use crate::ecs::OwnTask;

    fn listed(task_id: &str, connection: ConnectionState, age_seconds: Option<u64>) -> ListedTask {
        ListedTask {
            task: OwnTask {
                task_arn: format!("arn:aws:ecs:us-east-1:123456789012:task/c/{task_id}"),
                last_status: "RUNNING".into(),
                task_definition_arn: "arn:aws:ecs:us-east-1:123456789012:task-definition/worker:42"
                    .into(),
                created_at: age_seconds.map(|age| now() - Duration::from_secs(age)),
                started_at: None,
                is_run: connection == ConnectionState::Run,
                cpu: None,
                memory: None,
            },
            connection,
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    /// 呼ばれた内容を記録し、決めておいた選択を返す
    #[derive(Default)]
    struct FakeConsole {
        is_terminal: bool,
        selection: Option<usize>,
        shown_items: Option<Vec<String>>,
    }

    impl Console for FakeConsole {
        fn stdin_is_terminal(&self) -> bool {
            self.is_terminal
        }

        fn select(&mut self, _message: &str, items: Vec<String>) -> Result<Option<usize>> {
            self.shown_items = Some(items);
            Ok(self.selection)
        }

        fn multi_select(
            &mut self,
            _message: &str,
            _items: Vec<String>,
            _defaults: &[usize],
        ) -> Result<Option<Vec<usize>>> {
            unreachable!("開くものは 1 つだけ選ぶ")
        }

        fn read_line(&mut self, _question: &str) -> Result<String> {
            unreachable!("開くときは y/N を聞かない")
        }
    }

    fn terminal_selecting(index: Option<usize>) -> FakeConsole {
        FakeConsole {
            is_terminal: true,
            selection: index,
            ..FakeConsole::default()
        }
    }

    #[test]
    fn list_shows_id_connection_status_age_and_task_definition_in_aligned_columns() {
        let tasks = [
            listed("0123456789abcdef", ConnectionState::Connected, Some(125)),
            listed("89abcdef01234567", ConnectionState::Run, None),
        ];
        let mut console = terminal_selecting(Some(1));

        let chosen = choose_task(&tasks, &mut console, now()).unwrap();

        assert_eq!(chosen, Some(&tasks[1]));
        assert_eq!(
            console.shown_items.unwrap(),
            [
                "01234567  接続中  RUNNING  2 分 5 秒  worker:42",
                "89abcdef  run     RUNNING  ─          worker:42",
            ]
        );
    }

    #[test]
    fn single_task_is_opened_without_showing_the_list_even_without_terminal() {
        let tasks = [listed("0123456789abcdef", ConnectionState::Run, None)];
        let mut console = FakeConsole::default();

        assert_eq!(
            choose_task(&tasks, &mut console, now()).unwrap(),
            Some(&tasks[0])
        );
        assert_eq!(console.shown_items, None);
    }

    #[test]
    fn nothing_is_chosen_without_running_tasks() {
        assert_eq!(
            choose_task(&[], &mut FakeConsole::default(), now()).unwrap(),
            None
        );
    }

    #[test]
    fn list_of_several_tasks_without_terminal_is_an_error() {
        let tasks = [
            listed("0123456789abcdef", ConnectionState::Run, None),
            listed("89abcdef01234567", ConnectionState::Run, None),
        ];

        assert!(choose_task(&tasks, &mut FakeConsole::default(), now()).is_err());
    }

    #[test]
    fn escaping_either_list_cancels_with_exit_code_1() {
        let tasks = [
            listed("0123456789abcdef", ConnectionState::Run, None),
            listed("89abcdef01234567", ConnectionState::Run, None),
        ];
        let cancelled = |error: anyhow::Error| error.downcast::<Abort>().unwrap().exit_code();

        let task_error = choose_task(&tasks, &mut terminal_selecting(None), now()).unwrap_err();
        let page_error = choose_page(&mut terminal_selecting(None)).unwrap_err();

        assert_eq!(cancelled(task_error), 1);
        assert_eq!(cancelled(page_error), 1);
    }

    #[test]
    fn exec_task_opens_its_details_without_asking() {
        assert_eq!(decided_page(None, false).unwrap(), Some(Page::Task));
        assert_eq!(
            decided_page(Some(Page::Task), false).unwrap(),
            Some(Page::Task)
        );
    }

    #[test]
    fn logs_of_an_exec_task_are_an_error_pointing_to_task() {
        let message = decided_page(Some(Page::Logs), false)
            .unwrap_err()
            .to_string();

        assert!(message.contains("--task"), "{message}");
    }

    #[test]
    fn run_task_asks_unless_the_page_is_given() {
        assert_eq!(decided_page(None, true).unwrap(), None);
        assert_eq!(
            decided_page(Some(Page::Task), true).unwrap(),
            Some(Page::Task)
        );
        assert_eq!(
            decided_page(Some(Page::Logs), true).unwrap(),
            Some(Page::Logs)
        );
    }

    #[test]
    fn page_is_chosen_from_task_details_and_logs() {
        let mut console = terminal_selecting(Some(1));

        assert_eq!(choose_page(&mut console).unwrap(), Page::Logs);
        assert_eq!(
            console.shown_items.unwrap(),
            ["タスクの詳細", "ログ（CloudWatch Logs）"]
        );
    }

    #[test]
    fn page_cannot_be_chosen_without_terminal_and_points_to_the_flags() {
        let message = choose_page(&mut FakeConsole::default())
            .unwrap_err()
            .to_string();

        assert!(message.contains("--task か --logs"), "{message}");
    }

    #[test]
    fn flags_select_the_page_to_open() {
        assert_eq!(Page::from_flags(false, false), None);
        assert_eq!(Page::from_flags(true, false), Some(Page::Task));
        assert_eq!(Page::from_flags(false, true), Some(Page::Logs));
    }
}
