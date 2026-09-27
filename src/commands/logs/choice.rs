use std::fmt::Display;

use anyhow::{Result, bail};
use chrono::TimeZone;

use super::super::run::command_line;
use crate::history::Entry;
use crate::prompt::{Abort, Console};
use crate::ui;

/// 見る 1 件を決める。候補は新しい順。`last` なら選ばせずに先頭。候補が無ければ None
pub fn choose<'a, Tz: TimeZone>(
    candidates: &[&'a Entry],
    last: bool,
    console: &mut impl Console,
    time_zone: &Tz,
) -> Result<Option<&'a Entry>>
where
    Tz::Offset: Display,
{
    let Some(&newest) = candidates.first() else {
        return Ok(None);
    };
    if last {
        return Ok(Some(newest));
    }
    if !console.stdin_is_terminal() {
        bail!(
            "標準入力がターミナルではないため、履歴を一覧から選べません。直近の 1 件なら --last を付けてください"
        );
    }
    match console.select("見る run を選んでください", labels(candidates, time_zone))? {
        Some(index) => Ok(Some(candidates[index])),
        None => Err(Abort::Cancelled("ログの選択を取りやめました").into()),
    }
}

/// `2026-09-28 14:05  終了 0  2 分 5 秒  bundle exec rake db:migrate:status`
fn labels<Tz: TimeZone>(entries: &[&Entry], time_zone: &Tz) -> Vec<String>
where
    Tz::Offset: Display,
{
    let columns: Vec<[String; 3]> = entries
        .iter()
        .map(|entry| {
            [
                launched_at(entry, time_zone),
                state(entry),
                entry
                    .finish
                    .as_ref()
                    .map_or("─".to_owned(), |finish| ui::duration(finish.took())),
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
    let widths = [width(0), width(1), width(2)];
    columns
        .iter()
        .zip(entries)
        .map(|(row, entry)| {
            let cells: Vec<String> = row
                .iter()
                .zip(widths)
                .map(|(cell, width)| pad(cell, width))
                .collect();
            format!("{}  {}", cells.join("  "), command_line(&entry.command))
        })
        .collect()
}

pub fn launched_at<Tz: TimeZone>(entry: &Entry, time_zone: &Tz) -> String
where
    Tz::Offset: Display,
{
    entry
        .launched_at
        .with_timezone(time_zone)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn state(entry: &Entry) -> String {
    match &entry.finish {
        Some(finish) => match finish.exit_code {
            Some(code) => format!("終了 {code}"),
            None => "終了コードなし".to_owned(),
        },
        None => "未確認".to_owned(),
    }
}

fn pad(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(ui::display_width(text));
    format!("{text}{}", " ".repeat(fill))
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;

    use super::*;
    use crate::history::Finish;
    use crate::history::tests::entry;

    fn jst() -> FixedOffset {
        FixedOffset::east_opt(9 * 3600).unwrap()
    }

    fn finished(mut entry: Entry, exit_code: Option<i32>, took_seconds: u64) -> Entry {
        entry.finish = Some(Finish {
            exit_code,
            stopped_reason: None,
            container_reason: None,
            took_seconds,
        });
        entry
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
            unreachable!("履歴は 1 件だけ選ぶ")
        }

        fn read_line(&mut self, _question: &str) -> Result<String> {
            unreachable!("履歴を選ぶときは y/N を聞かない")
        }
    }

    #[test]
    fn list_shows_local_time_state_time_taken_and_command_in_aligned_columns() {
        let (newer, older) = (
            finished(entry("staging", "bbb", 5), Some(0), 125),
            entry("staging", "aaa", 0),
        );
        let mut console = FakeConsole {
            is_terminal: true,
            selection: Some(1),
            ..FakeConsole::default()
        };

        let chosen = choose(&[&newer, &older], false, &mut console, &jst()).unwrap();

        assert_eq!(chosen, Some(&older));
        assert_eq!(
            console.shown_items.unwrap(),
            [
                "2026-09-28 14:05  終了 0  2 分 5 秒  bundle exec rake",
                "2026-09-28 14:00  未確認  ─          bundle exec rake",
            ]
        );
    }

    #[test]
    fn state_shows_the_exit_code_or_that_the_result_is_not_known() {
        let states = [
            finished(entry("staging", "a", 0), Some(137), 1),
            finished(entry("staging", "b", 0), None, 1),
            entry("staging", "c", 0),
        ]
        .map(|entry| state(&entry));

        assert_eq!(states, ["終了 137", "終了コードなし", "未確認"]);
    }

    #[test]
    fn last_opens_the_newest_without_showing_the_list_even_without_terminal() {
        let (newer, older) = (entry("staging", "bbb", 5), entry("staging", "aaa", 0));
        let mut console = FakeConsole::default();

        let chosen = choose(&[&newer, &older], true, &mut console, &jst()).unwrap();

        assert_eq!(chosen, Some(&newer));
        assert_eq!(console.shown_items, None);
    }

    #[test]
    fn nothing_is_chosen_without_history() {
        for last in [false, true] {
            let chosen = choose(&[], last, &mut FakeConsole::default(), &jst()).unwrap();

            assert_eq!(chosen, None, "last={last}");
        }
    }

    #[test]
    fn list_without_terminal_is_an_error_suggesting_last() {
        let only = entry("staging", "aaa", 0);

        let message = choose(&[&only], false, &mut FakeConsole::default(), &jst())
            .unwrap_err()
            .to_string();

        assert!(message.contains("--last"), "{message}");
    }

    #[test]
    fn escaping_the_list_cancels_with_exit_code_1() {
        let only = entry("staging", "aaa", 0);
        let mut console = FakeConsole {
            is_terminal: true,
            ..FakeConsole::default()
        };

        let abort = choose(&[&only], false, &mut console, &jst())
            .unwrap_err()
            .downcast::<Abort>()
            .unwrap();

        assert_eq!(abort, Abort::Cancelled("ログの選択を取りやめました"));
        assert_eq!(abort.exit_code(), 1);
    }
}
