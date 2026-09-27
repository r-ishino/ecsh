//! ps の一覧の表。列の幅を全角込みで揃え、気にするべきタスクに色を付ける

use std::time::{Duration, SystemTime};

use super::listing::{ConnectionState, ListedTask};
use crate::ecs::{self, OwnTask};
use crate::ui::{self, Style};

pub struct Row<'a> {
    pub profile: &'a str,
    pub task: &'a ListedTask,
}

const HEADERS: [&str; 8] = [
    "ID",
    "接続",
    "状態",
    "起動から",
    "タスク定義",
    "CPU",
    "メモリ",
    "自動停止まで",
];
const PROFILE_HEADER: &str = "プロファイル";
const NOT_APPLICABLE: &str = "─";
const COLUMN_GAP: &str = "  ";

/// 接続が不明のタスクを、起動からこの時間を超えたら黄にする
const UNKNOWN_CAUTION_AFTER: Duration = Duration::from_secs(60 * 60);
/// 接続が不明のタスクを、起動からこの時間を超えたら赤にする
const UNKNOWN_ALERT_AFTER: Duration = Duration::from_secs(2 * 60 * 60);
const AUTO_STOP_AFTER: Duration = Duration::from_secs(ecs::KEEPALIVE_SECONDS);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Tone {
    Plain,
    Caution,
    Alert,
}

#[derive(Debug, PartialEq)]
struct Cell {
    text: String,
    tone: Tone,
}

impl Cell {
    fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

/// 見出しの行と、rows の 1 件ごとの行。with_profile なら先頭にプロファイルの列を付ける
pub fn render(rows: &[Row<'_>], with_profile: bool, now: SystemTime, style: Style) -> Vec<String> {
    let header = with_profile
        .then_some(PROFILE_HEADER)
        .into_iter()
        .chain(HEADERS)
        .map(|header| Cell::new(header, Tone::Plain))
        .collect::<Vec<_>>();
    let lines: Vec<Vec<Cell>> = std::iter::once(header)
        .chain(rows.iter().map(|row| cells(row, with_profile, now)))
        .collect();

    let mut widths = vec![0; lines[0].len()];
    for line in &lines {
        for (width, cell) in widths.iter_mut().zip(line) {
            *width = (*width).max(ui::display_width(&cell.text));
        }
    }
    lines
        .iter()
        .map(|line| format_line(line, &widths, style))
        .collect()
}

fn format_line(cells: &[Cell], widths: &[usize], style: Style) -> String {
    let last = cells.len() - 1;
    cells
        .iter()
        .zip(widths)
        .enumerate()
        .map(|(index, (cell, width))| {
            let painted = paint(style, cell);
            if index == last {
                painted
            } else {
                let padding = width - ui::display_width(&cell.text);
                format!("{painted}{}", " ".repeat(padding))
            }
        })
        .collect::<Vec<_>>()
        .join(COLUMN_GAP)
}

fn paint(style: Style, cell: &Cell) -> String {
    match cell.tone {
        Tone::Plain => cell.text.clone(),
        Tone::Caution => style.yellow(&cell.text),
        Tone::Alert => style.red(&cell.text),
    }
}

fn cells(row: &Row<'_>, with_profile: bool, now: SystemTime) -> Vec<Cell> {
    let ListedTask { task, connection } = row.task;
    let row_tone = match connection {
        ConnectionState::Abandoned => Tone::Caution,
        _ => Tone::Plain,
    };
    let elapsed = task.created_at.map(|at| elapsed_since(at, now));
    let elapsed_tone = row_tone.max(elapsed_tone(*connection, elapsed));

    let mut cells = Vec::with_capacity(HEADERS.len() + 1);
    if with_profile {
        cells.push(Cell::new(row.profile, row_tone));
    }
    cells.extend([
        Cell::new(ui::short_task_id(&task.task_arn), row_tone),
        Cell::new(connection.label(), row_tone),
        Cell::new(&task.last_status, row_tone),
        Cell::new(
            elapsed.map_or(NOT_APPLICABLE.to_owned(), ui::duration),
            elapsed_tone,
        ),
        Cell::new(
            ui::task_definition_name(&task.task_definition_arn),
            row_tone,
        ),
        Cell::new(or_not_applicable(task.cpu), row_tone),
        Cell::new(or_not_applicable(task.memory), row_tone),
        Cell::new(until_auto_stop_text(task, now), row_tone),
    ]);
    cells
}

fn or_not_applicable(value: Option<impl ToString>) -> String {
    value.map_or(NOT_APPLICABLE.to_owned(), |value| value.to_string())
}

/// 時計が戻って未来の時刻になっていたら 0 とみなす
fn elapsed_since(at: SystemTime, now: SystemTime) -> Duration {
    now.duration_since(at).unwrap_or_default()
}

/// 接続中と run は長く使うのが普通なので、時間で色を付けるのは不明のタスクだけ
fn elapsed_tone(connection: ConnectionState, elapsed: Option<Duration>) -> Tone {
    match (connection, elapsed) {
        (ConnectionState::Unknown, Some(elapsed)) if elapsed > UNKNOWN_ALERT_AFTER => Tone::Alert,
        (ConnectionState::Unknown, Some(elapsed)) if elapsed > UNKNOWN_CAUTION_AFTER => {
            Tone::Caution
        }
        _ => Tone::Plain,
    }
}

/// 12 時間の sleep はコンテナが動き始めてから数えるので、起動時刻（startedAt）から逆算する。run のタスクは sleep しない
fn until_auto_stop_text(task: &OwnTask, now: SystemTime) -> String {
    let Some(started_at) = task.started_at.filter(|_| !task.is_run) else {
        return NOT_APPLICABLE.to_owned();
    };
    match AUTO_STOP_AFTER.saturating_sub(elapsed_since(started_at, now)) {
        Duration::ZERO => "まもなく".to_owned(),
        remaining => ui::duration(remaining),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::size::{Cpu, Memory};

    const NOW_SECS: u64 = 1_700_000_000;

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(NOW_SECS)
    }

    fn ago(elapsed: Duration) -> SystemTime {
        now() - elapsed
    }

    fn minutes(minutes: u64) -> Duration {
        Duration::from_secs(minutes * 60)
    }

    fn listed(connection: ConnectionState, created_ago: Duration) -> ListedTask {
        ListedTask {
            task: OwnTask {
                task_arn: "arn:aws:ecs:us-east-1:123456789012:task/c/0123456789abcdef".into(),
                last_status: "RUNNING".into(),
                task_definition_arn: "arn:aws:ecs:us-east-1:123456789012:task-definition/worker:42"
                    .into(),
                created_at: Some(ago(created_ago)),
                started_at: Some(ago(created_ago.saturating_sub(Duration::from_secs(30)))),
                is_run: connection == ConnectionState::Run,
                cpu: Some(Cpu::from_units(1024)),
                memory: Some(Memory::from_mib(2048)),
            },
            connection,
        }
    }

    fn row(task: &ListedTask) -> Row<'_> {
        Row {
            profile: "staging",
            task,
        }
    }

    fn tones(task: &ListedTask) -> Vec<Tone> {
        cells(&row(task), false, now())
            .into_iter()
            .map(|cell| cell.tone)
            .collect()
    }

    fn elapsed_tone_after(connection: ConnectionState, elapsed: Duration) -> Tone {
        tones(&listed(connection, elapsed))[3]
    }

    #[test]
    fn table_aligns_columns_counting_full_width_characters_as_two() {
        let connected = listed(ConnectionState::Connected, minutes(12));
        let mut abandoned = listed(ConnectionState::Abandoned, minutes(125));
        abandoned.task.task_arn =
            "arn:aws:ecs:us-east-1:123456789012:task/c/fedcba9876543210".into();
        abandoned.task.task_definition_arn =
            "arn:aws:ecs:us-east-1:123456789012:task-definition/console:7".into();
        abandoned.task.cpu = Some(Cpu::from_units(512));
        abandoned.task.memory = Some(Memory::from_mib(4096));

        let lines = render(
            &[row(&connected), row(&abandoned)],
            false,
            now(),
            Style::PLAIN,
        );

        assert_eq!(
            lines,
            [
                "ID        接続      状態     起動から     タスク定義  CPU       メモリ  自動停止まで",
                "01234567  接続中    RUNNING  12 分        worker:42   1 vCPU    2 GB    11 時間 48 分",
                "fedcba98  止め忘れ  RUNNING  2 時間 5 分  console:7   0.5 vCPU  4 GB    9 時間 55 分",
            ]
        );
    }

    #[test]
    fn all_profiles_table_starts_with_the_profile_column() {
        let task = listed(ConnectionState::Connected, minutes(12));

        let lines = render(&[row(&task)], true, now(), Style::PLAIN);

        assert!(lines[0].starts_with("プロファイル  ID  "), "{}", lines[0]);
        assert!(
            lines[1].starts_with("staging       01234567  "),
            "{}",
            lines[1]
        );
    }

    #[test]
    fn abandoned_task_is_yellow_across_the_row() {
        let task = listed(ConnectionState::Abandoned, minutes(10));

        assert_eq!(tones(&task), [Tone::Caution; 8]);
        assert_eq!(
            cells(&row(&task), true, now())[0].tone,
            Tone::Caution,
            "プロファイルの列も黄"
        );
    }

    #[test]
    fn unknown_task_turns_yellow_after_1_hour_and_red_after_2_hours() {
        let unknown = ConnectionState::Unknown;

        assert_eq!(elapsed_tone_after(unknown, minutes(60)), Tone::Plain);
        assert_eq!(elapsed_tone_after(unknown, minutes(61)), Tone::Caution);
        assert_eq!(elapsed_tone_after(unknown, minutes(120)), Tone::Caution);
        assert_eq!(elapsed_tone_after(unknown, minutes(121)), Tone::Alert);
    }

    #[test]
    fn only_the_elapsed_column_of_an_unknown_task_is_colored() {
        let tones = tones(&listed(ConnectionState::Unknown, minutes(180)));

        assert_eq!(
            tones,
            [
                Tone::Plain,
                Tone::Plain,
                Tone::Plain,
                Tone::Alert,
                Tone::Plain,
                Tone::Plain,
                Tone::Plain,
                Tone::Plain
            ]
        );
    }

    #[test]
    fn connected_and_run_tasks_are_not_colored_however_long_they_run() {
        for connection in [ConnectionState::Connected, ConnectionState::Run] {
            assert_eq!(
                tones(&listed(connection, minutes(600))),
                [Tone::Plain; 8],
                "{connection:?}"
            );
        }
    }

    #[test]
    fn colored_cells_are_padded_outside_the_escape_sequences() {
        let task = listed(ConnectionState::Abandoned, minutes(10));
        let color = Style::COLOR;

        let lines = render(&[row(&task)], false, now(), color);

        let elapsed_then_task_definition = format!(
            "{}     {}",
            color.yellow("10 分"),
            color.yellow("worker:42")
        );
        assert!(
            lines[1].contains(&elapsed_then_task_definition),
            "{:?}",
            lines[1]
        );
    }

    #[test]
    fn auto_stop_counts_down_12_hours_from_when_the_task_started() {
        let mut task = listed(ConnectionState::Connected, minutes(0)).task;
        task.started_at = Some(ago(minutes(90)));

        assert_eq!(until_auto_stop_text(&task, now()), "10 時間 30 分");
    }

    #[test]
    fn auto_stop_is_imminent_once_12_hours_have_passed() {
        let mut task = listed(ConnectionState::Unknown, minutes(0)).task;
        task.started_at = Some(ago(minutes(12 * 60 + 5)));

        assert_eq!(until_auto_stop_text(&task, now()), "まもなく");
    }

    #[test]
    fn auto_stop_is_not_shown_before_the_task_starts_or_for_run_tasks() {
        let mut pending = listed(ConnectionState::Unknown, minutes(1)).task;
        pending.started_at = None;
        let run = listed(ConnectionState::Run, minutes(10)).task;

        assert_eq!(until_auto_stop_text(&pending, now()), "─");
        assert_eq!(until_auto_stop_text(&run, now()), "─");
    }

    #[test]
    fn size_is_not_shown_when_the_task_has_none() {
        let mut task = listed(ConnectionState::Connected, minutes(1));
        task.task.cpu = None;
        task.task.memory = None;

        let cells = cells(&row(&task), false, now());

        assert_eq!(cells[5], Cell::new("─", Tone::Plain));
        assert_eq!(cells[6], Cell::new("─", Tone::Plain));
    }

    #[test]
    fn elapsed_is_not_shown_without_a_creation_time() {
        let mut task = listed(ConnectionState::Unknown, minutes(1));
        task.task.created_at = None;

        let cells = cells(&row(&task), false, now());

        assert_eq!(cells[3], Cell::new("─", Tone::Plain));
    }
}
