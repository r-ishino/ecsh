//! run の履歴を `<state_dir>/history.jsonl` に 1 行 1 件で持つ。logs はここから選ぶ

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::logs::LogStream;
use crate::xdg;

/// 残す件数。これを超えたら古いものから消す
pub const LIMIT: usize = 100;

/// run 1 回分
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub launched_at: DateTime<Utc>,
    pub profile: String,
    pub region: String,
    pub cluster: String,
    /// 起動に使った AWS プロファイル。SDK の既定に任せたときは無い
    pub aws_profile: Option<String>,
    pub container: String,
    pub task_arn: String,
    pub command: Vec<String>,
    /// 出力を CloudWatch Logs から読めない設定だったときは無い
    pub log: Option<LogStream>,
    /// 終わりを見届けるまでは無い
    #[serde(default)]
    pub finish: Option<Finish>,
}

/// 止まったタスクから読み取った結果
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finish {
    pub exit_code: Option<i32>,
    pub stopped_reason: Option<String>,
    pub container_reason: Option<String>,
    pub took_seconds: u64,
}

impl Finish {
    pub fn took(&self) -> Duration {
        Duration::from_secs(self.took_seconds)
    }
}

/// 履歴の読み書き。置き場所をテストで差し替えられるように、ディレクトリを持つ
pub struct History {
    dir: PathBuf,
}

impl History {
    pub fn open() -> Result<Self> {
        let dir = xdg::state_dir().ok_or_else(|| anyhow!("XDG_STATE_HOME も HOME も未設定です"))?;
        Ok(Self::at(dir))
    }

    fn at(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self) -> PathBuf {
        self.dir.join("history.jsonl")
    }

    /// 古い順の全件。ファイルがまだ無ければ空
    pub fn load(&self) -> Result<Vec<Entry>> {
        let path = self.path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(anyhow::Error::new(error)
                    .context(format!("履歴を読めません: {}", path.display())));
            }
        };
        parse(&text).with_context(|| format!("履歴の形式が不正です: {}", path.display()))
    }

    /// 1 件足し、LIMIT を超えた古いものを消す
    pub fn append(&self, entry: Entry) -> Result<()> {
        self.rewrite(|entries| {
            entries.push(entry);
            let excess = entries.len().saturating_sub(LIMIT);
            entries.drain(..excess);
        })
    }

    /// task_arn の履歴に結果を書く。もう消えていれば何もしない
    pub fn record_finish(&self, task_arn: &str, finish: Finish) -> Result<()> {
        self.rewrite(|entries| {
            if let Some(entry) = entries.iter_mut().find(|entry| entry.task_arn == task_arn) {
                entry.finish = Some(finish);
            }
        })
    }

    fn rewrite(&self, change: impl FnOnce(&mut Vec<Entry>)) -> Result<()> {
        fs::create_dir_all(&self.dir)
            .with_context(|| format!("ディレクトリを作れません: {}", self.dir.display()))?;
        let _lock = self.lock()?;
        let mut entries = self.load()?;
        change(&mut entries);
        let path = self.path();
        let temporary = self.dir.join("history.jsonl.tmp");
        fs::write(&temporary, serialize(&entries)?)
            .with_context(|| format!("履歴を書けません: {}", temporary.display()))?;
        // 途中で落ちても、読み手が書きかけのファイルを見ないように置き換える
        fs::rename(&temporary, &path)
            .with_context(|| format!("履歴を置き換えられません: {}", path.display()))
    }

    /// 並行する run どうしで、読んでから書くまでの間に互いの追記を消さないためのロック
    fn lock(&self) -> Result<File> {
        let path = self.dir.join("history.lock");
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("ロックファイルを作れません: {}", path.display()))?;
        file.lock()
            .with_context(|| format!("ロックを取れません: {}", path.display()))?;
        Ok(file)
    }
}

fn parse(text: &str) -> Result<Vec<Entry>> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).with_context(|| format!("{} 行目を読めません", index + 1))
        })
        .collect()
}

fn serialize(entries: &[Entry]) -> Result<String> {
    let mut text = String::new();
    for entry in entries {
        text.push_str(&serde_json::to_string(entry)?);
        text.push('\n');
    }
    Ok(text)
}

/// profile の履歴を新しい順に。profile が無ければ全プロファイル
pub fn newest_first<'a>(entries: &'a [Entry], profile: Option<&str>) -> Vec<&'a Entry> {
    entries
        .iter()
        .rev()
        .filter(|entry| profile.is_none_or(|name| entry.profile == name))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    pub(crate) fn entry(profile: &str, task_id: &str, minute: u32) -> Entry {
        Entry {
            launched_at: format!("2026-09-28T05:{minute:02}:00Z").parse().unwrap(),
            profile: profile.into(),
            region: "us-east-1".into(),
            cluster: format!("example-{profile}"),
            aws_profile: Some("example".into()),
            container: "app".into(),
            task_arn: format!(
                "arn:aws:ecs:us-east-1:123456789012:task/example-{profile}/{task_id}"
            ),
            command: vec!["bundle".into(), "exec".into(), "rake".into()],
            log: Some(LogStream {
                region: "us-east-1".into(),
                group: "/ecs/worker".into(),
                stream: format!("ecs/app/{task_id}"),
            }),
            finish: None,
        }
    }

    fn finish(exit_code: i32) -> Finish {
        Finish {
            exit_code: Some(exit_code),
            stopped_reason: Some("Essential container in task exited".into()),
            container_reason: None,
            took_seconds: 125,
        }
    }

    /// テストごとに別の空ディレクトリ。消すのは OS の一時ディレクトリの掃除に任せる
    fn temp_history() -> History {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ecsh-history-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        History::at(dir)
    }

    #[test]
    fn history_is_empty_before_the_first_run() {
        assert_eq!(temp_history().load().unwrap(), []);
    }

    #[test]
    fn appended_entries_are_read_back_oldest_first() {
        let history = temp_history();
        let (first, second) = (entry("staging", "aaa", 0), entry("production", "bbb", 1));

        history.append(first.clone()).unwrap();
        history.append(second.clone()).unwrap();

        assert_eq!(history.load().unwrap(), [first, second]);
    }

    #[test]
    fn only_the_newest_100_entries_are_kept() {
        let history = temp_history();

        for index in 0..LIMIT + 2 {
            history
                .append(entry("staging", &format!("task{index}"), 0))
                .unwrap();
        }

        let entries = history.load().unwrap();
        assert_eq!(entries.len(), LIMIT);
        assert!(entries[0].task_arn.ends_with("/task2"));
        assert!(
            entries[LIMIT - 1]
                .task_arn
                .ends_with(&format!("/task{}", LIMIT + 1))
        );
    }

    #[test]
    fn finish_is_recorded_only_on_the_entry_of_that_task() {
        let history = temp_history();
        let (target, other) = (entry("staging", "aaa", 0), entry("staging", "bbb", 1));
        history.append(target.clone()).unwrap();
        history.append(other.clone()).unwrap();

        history.record_finish(&target.task_arn, finish(3)).unwrap();

        let entries = history.load().unwrap();
        assert_eq!(entries[0].finish, Some(finish(3)));
        assert_eq!(entries[1], other);
    }

    #[test]
    fn finish_of_an_entry_already_trimmed_away_is_dropped() {
        let history = temp_history();
        let kept = entry("staging", "aaa", 0);
        history.append(kept.clone()).unwrap();

        history
            .record_finish("arn:aws:ecs:us-east-1:123456789012:task/c/gone", finish(0))
            .unwrap();

        assert_eq!(history.load().unwrap(), [kept]);
    }

    #[test]
    fn broken_line_is_an_error_naming_the_line() {
        let text = format!(
            "{}\n{{broken\n",
            serde_json::to_string(&entry("staging", "aaa", 0)).unwrap()
        );

        let message = format!("{:#}", parse(&text).unwrap_err());

        assert!(message.contains("2 行目"), "{message}");
    }

    #[test]
    fn entry_written_before_it_finished_is_read_without_a_finish() {
        let mut value = serde_json::to_value(entry("staging", "aaa", 0)).unwrap();
        value.as_object_mut().unwrap().remove("finish");

        let entries = parse(&value.to_string()).unwrap();

        assert_eq!(entries[0].finish, None);
    }

    #[test]
    fn newest_first_filters_by_profile_and_reverses_the_order() {
        let entries = [
            entry("staging", "aaa", 0),
            entry("production", "bbb", 1),
            entry("staging", "ccc", 2),
        ];

        let ids = |selected: Vec<&Entry>| -> Vec<String> {
            selected
                .iter()
                .map(|entry| crate::ui::task_id(&entry.task_arn).to_owned())
                .collect()
        };

        assert_eq!(ids(newest_first(&entries, Some("staging"))), ["ccc", "aaa"]);
        assert_eq!(ids(newest_first(&entries, None)), ["ccc", "bbb", "aaa"]);
        assert!(newest_first(&entries, Some("dev")).is_empty());
    }
}
