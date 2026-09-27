//! 出力の見た目。色と記号を付け、状態の更新をスピナー 1 行にまとめる

use std::env;
use std::ffi::OsStr;
use std::fmt::{self, Display};
use std::io::{self, IsTerminal};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressState, ProgressStyle};
use unicode_width::UnicodeWidthStr;

use crate::report::report;

/// stderr に色やスピナーを出すかどうか
pub fn color_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        should_color(
            io::stderr().is_terminal(),
            env::var_os("NO_COLOR").as_deref(),
        )
    })
}

/// stdout に色を出すかどうか。ps の一覧はパイプで後段に渡すので、stderr とは別に決める
fn stdout_color_enabled() -> bool {
    should_color(
        io::stdout().is_terminal(),
        env::var_os("NO_COLOR").as_deref(),
    )
}

/// NO_COLOR は https://no-color.org/ に従い、空でない値が入っているときだけ効く
fn should_color(stderr_is_terminal: bool, no_color: Option<&OsStr>) -> bool {
    stderr_is_terminal && no_color.is_none_or(OsStr::is_empty)
}

/// 色を付けるかを決めた上で、1 行分の文字列を組み立てる
#[derive(Debug, Clone, Copy)]
pub struct Style {
    color: bool,
}

impl Style {
    #[cfg(test)]
    pub const PLAIN: Self = Self { color: false };

    #[cfg(test)]
    pub const COLOR: Self = Self { color: true };

    pub fn current() -> Self {
        Self {
            color: color_enabled(),
        }
    }

    /// stdout に出す行のための Style
    pub fn for_stdout() -> Self {
        Self {
            color: stdout_color_enabled(),
        }
    }

    fn paint(self, sgr: &str, text: impl Display) -> String {
        if self.color {
            format!("\x1b[{sgr}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    pub fn bold(self, text: impl Display) -> String {
        self.paint("1", text)
    }

    fn dim(self, text: impl Display) -> String {
        self.paint("2", text)
    }

    pub fn red(self, text: impl Display) -> String {
        self.paint("31", text)
    }

    fn green(self, text: impl Display) -> String {
        self.paint("32", text)
    }

    pub fn yellow(self, text: impl Display) -> String {
        self.paint("33", text)
    }

    /// `✓ タスクを起動しました`
    pub fn success(self, message: impl Display) -> String {
        format!("{} {message}", self.green("✓"))
    }

    /// `✗ ...`。失敗の要約
    pub fn failure(self, message: impl Display) -> String {
        format!("{} {message}", self.red("✗"))
    }

    /// `! ...`。タスクが残るかもしれないなど、読み流してほしくないこと
    pub fn warning(self, message: impl Display) -> String {
        self.yellow(format!("! {message}"))
    }

    /// 字下げした補足。薄く出す
    pub fn note(self, message: impl Display) -> String {
        format!("  {}", self.dim(message))
    }

    /// 字下げした対処。注意と同じ黄で出す
    fn remedy(self, message: impl Display) -> String {
        format!("  {}", self.yellow(message))
    }

    /// エラーを「✗ 要約 → 対処 → 元のエラーの連鎖（薄く）」の行にする
    pub fn error_lines(self, error: &anyhow::Error) -> Vec<String> {
        let mut chain = error.chain().map(ToString::to_string);
        let summary = chain.next().unwrap_or_default();
        let mut lines = vec![self.failure(summary)];
        lines.extend(remedies_for(error).map(|remedy| self.remedy(remedy)));
        lines.extend(self.cause_lines(chain));
        lines
    }

    /// 処理を続けられた失敗を「! 見出し → 対処 → エラーの連鎖（薄く）」の行にする
    pub fn warning_lines(self, headline: impl Display, error: &anyhow::Error) -> Vec<String> {
        let mut lines = vec![self.warning(headline)];
        lines.extend(remedies_for(error).map(|remedy| self.remedy(remedy)));
        lines.extend(self.cause_lines(error.chain().map(ToString::to_string)));
        lines
    }

    fn cause_lines(self, causes: impl Iterator<Item = String>) -> impl Iterator<Item = String> {
        causes
            .flat_map(|cause| cause.lines().map(str::to_owned).collect::<Vec<_>>())
            .map(move |cause| self.note(cause))
    }
}

/// エラーを「✗ 要約 → 対処 → 元のエラーの連鎖（薄く）」で stderr に出す
pub fn report_error(error: &anyhow::Error) {
    for line in Style::current().error_lines(error) {
        report!("{line}");
    }
}

/// エラーに添える対処。連鎖の外側のものから順に、重なっていればすべて
fn remedies_for(error: &anyhow::Error) -> impl Iterator<Item = &str> {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<Guidance>())
        .map(|guidance| guidance.remedy.as_str())
}

/// 次にどうすればよいかを添えたエラー。表示は要約だけで、対処は `Style::error_lines` が別の行に出す
#[derive(Debug)]
pub struct Guidance {
    summary: String,
    remedy: String,
    cause: Option<anyhow::Error>,
}

impl Guidance {
    pub fn new(summary: impl Into<String>, remedy: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            remedy: remedy.into(),
            cause: None,
        }
    }

    /// 元のエラーの連鎖を残したまま、要約と対処を上に重ねる
    ///
    /// anyhow の context として付けないのは、context は `chain()` から型で取り出せず、対処が重なったときに外側の 1 つしか拾えないから
    pub fn wrap(self, cause: anyhow::Error) -> anyhow::Error {
        anyhow::Error::new(Self {
            cause: Some(cause),
            ..self
        })
    }
}

impl Display for Guidance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary)
    }
}

impl std::error::Error for Guidance {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_deref().map(|cause| cause as _)
    }
}

/// 状態が変わるのを待つ間の表示。ターミナルならスピナー 1 行を更新し続け、そうでなければ状態が変わったときだけ 1 行出す
///
/// drop でスピナーを消す。シグナルで待ちの future ごと捨てられても、エラーで抜けても行が残らない
pub struct Waiting {
    started: Instant,
    display: WaitingDisplay,
}

enum WaitingDisplay {
    Spinner(ProgressBar),
    Lines(ChangedLines),
}

impl Waiting {
    /// heading は待っている内容、limit はスピナーを出さないときに見出しに添える上限
    pub fn start(heading: &str, limit: Option<&str>) -> Self {
        let display = if color_enabled() {
            WaitingDisplay::Spinner(spinner(heading))
        } else {
            match limit {
                Some(limit) => report!("{heading}（{limit}）"),
                None => report!("{heading}"),
            }
            WaitingDisplay::Lines(ChangedLines::default())
        };
        Self {
            started: Instant::now(),
            display,
        }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// 今の状態を出す
    pub fn update(&mut self, status: String) {
        match &mut self.display {
            WaitingDisplay::Spinner(bar) => bar.set_message(status),
            WaitingDisplay::Lines(lines) => {
                let elapsed = self.started.elapsed();
                if let Some(status) = lines.changed(status) {
                    report!("[{}] {status}", clock(elapsed));
                }
            }
        }
    }

    /// スピナーを消し、結果の 1 行だけを残す
    pub fn finish(self, message: impl Display) {
        drop(self);
        report!("{}", Style::current().success(message));
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if let WaitingDisplay::Spinner(bar) = &self.display {
            bar.finish_and_clear();
        }
    }
}

fn spinner(heading: &str) -> ProgressBar {
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
    bar.set_style(spinner_style());
    bar.set_prefix(heading.to_owned());
    bar.enable_steady_tick(Duration::from_millis(100));
    bar
}

const SPINNER_TEMPLATE: &str = "{spinner} {prefix}  {clock}  {msg}";

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template(SPINNER_TEMPLATE)
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
        .with_key("clock", |state: &ProgressState, w: &mut dyn fmt::Write| {
            let _ = w.write_str(&clock(state.elapsed()));
        })
}

/// 直前と同じ状態を読み捨てる
#[derive(Default)]
struct ChangedLines {
    last: Option<String>,
}

impl ChangedLines {
    fn changed(&mut self, status: String) -> Option<String> {
        if self.last.as_ref() == Some(&status) {
            return None;
        }
        self.last = Some(status.clone());
        Some(status)
    }
}

/// `02:05`。待っている間の経過時間
pub fn clock(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

/// `45 秒`・`2 分 5 秒`・`1 時間 3 分`。1 時間を超えたら秒は出さない
pub fn duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    match (hours, minutes, seconds) {
        (0, 0, s) => format!("{s} 秒"),
        (0, m, 0) => format!("{m} 分"),
        (0, m, s) => format!("{m} 分 {s} 秒"),
        (h, 0, _) => format!("{h} 時間"),
        (h, m, _) => format!("{h} 時間 {m} 分"),
    }
}

/// 端末で占める幅。全角の文字は 2 と数える
pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// `arn:aws:ecs:<region>:<account>:task/<cluster>/<ID>` → `<ID>`
pub fn task_id(task_arn: &str) -> &str {
    task_arn.rsplit('/').next().unwrap_or(task_arn)
}

/// タスク ARN の ID の先頭 8 文字。ECS コンソールの一覧で見分けられる長さ
pub fn short_task_id(task_arn: &str) -> &str {
    let id = task_id(task_arn);
    id.get(..8).unwrap_or(id)
}

/// `arn:aws:ecs:<region>:<account>:task-definition/worker:42` → `worker:42`
pub fn task_definition_name(task_definition_arn: &str) -> &str {
    task_definition_arn
        .rsplit('/')
        .next()
        .unwrap_or(task_definition_arn)
}

/// `subnet-0123456789abcdef0` → `subnet-01234567…`。ネットワークの行を 1 行に収める
pub fn short_resource_id(id: &str) -> String {
    match id.split_once('-') {
        Some((kind, hex)) if hex.chars().count() > 8 => {
            format!("{kind}-{}…", hex.chars().take(8).collect::<String>())
        }
        _ => id.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, anyhow};

    use super::*;

    const COLOR: Style = Style::COLOR;
    const PLAIN: Style = Style::PLAIN;

    #[test]
    fn color_is_used_only_on_a_terminal_without_no_color() {
        assert!(should_color(true, None));
        assert!(!should_color(false, None));
        assert!(!should_color(true, Some(OsStr::new("1"))));
        assert!(!should_color(false, Some(OsStr::new("1"))));
    }

    #[test]
    fn empty_no_color_does_not_disable_color() {
        assert!(should_color(true, Some(OsStr::new(""))));
    }

    #[test]
    fn plain_style_emits_no_escape_sequences() {
        let error = Guidance::new("止められません", "gc").wrap(anyhow!("expired token"));
        let lines = [
            PLAIN.bold("staging"),
            PLAIN.success("起動しました"),
            PLAIN.warning("残るかもしれません"),
            PLAIN.note("補足"),
        ]
        .into_iter()
        .chain(PLAIN.error_lines(&error));

        for line in lines {
            assert!(!line.contains('\x1b'), "{line:?}");
        }
    }

    #[test]
    fn colored_style_wraps_text_in_sgr_and_resets() {
        assert_eq!(COLOR.bold("staging"), "\x1b[1mstaging\x1b[0m");
        assert_eq!(
            COLOR.success("起動しました"),
            "\x1b[32m✓\x1b[0m 起動しました"
        );
        assert_eq!(COLOR.note("補足"), "  \x1b[2m補足\x1b[0m");
    }

    #[test]
    fn result_lines_are_marked_with_check_cross_and_bang() {
        assert_eq!(PLAIN.success("止めました"), "✓ 止めました");
        assert_eq!(PLAIN.warning("待たずに終了"), "! 待たずに終了");
        assert_eq!(
            PLAIN.error_lines(&anyhow!("失敗しました")),
            ["✗ 失敗しました"]
        );
    }

    #[test]
    fn error_shows_summary_then_the_whole_cause_chain_indented() {
        let error = anyhow!("expired token")
            .context("StopTask に失敗しました")
            .context("タスクを止められません");

        assert_eq!(
            PLAIN.error_lines(&error),
            [
                "✗ タスクを止められません",
                "  StopTask に失敗しました",
                "  expired token",
            ]
        );
    }

    #[test]
    fn guidance_puts_the_remedy_between_summary_and_causes() {
        let error = Guidance::new("タスクを止められませんでした", "`ecsh gc` で止められます")
            .wrap(anyhow!("expired token"));

        assert_eq!(
            PLAIN.error_lines(&error),
            [
                "✗ タスクを止められませんでした",
                "  `ecsh gc` で止められます",
                "  expired token",
            ]
        );
    }

    #[test]
    fn nested_guidance_shows_every_remedy_outermost_first_and_keeps_inner_summaries() {
        let inner = Guidance::new("セッションが切れています", "ログインしてください")
            .wrap(anyhow!("expired token"))
            .context("StopTask に失敗しました");
        let error =
            Guidance::new("タスクを止められませんでした", "`ecsh gc` で止められます").wrap(inner);

        assert_eq!(
            PLAIN.error_lines(&error),
            [
                "✗ タスクを止められませんでした",
                "  `ecsh gc` で止められます",
                "  ログインしてください",
                "  StopTask に失敗しました",
                "  セッションが切れています",
                "  expired token",
            ]
        );
    }

    #[test]
    fn guidance_without_a_cause_shows_summary_and_remedy() {
        let error = anyhow::Error::new(Guidance::new("見つかりません", "インストールしてください"));

        assert_eq!(
            PLAIN.error_lines(&error),
            ["✗ 見つかりません", "  インストールしてください"]
        );
    }

    #[test]
    fn multi_line_causes_are_each_indented() {
        let error = Err::<(), _>(anyhow!("a\nb")).context("要約").unwrap_err();

        assert_eq!(PLAIN.error_lines(&error), ["✗ 要約", "  a", "  b"]);
    }

    #[test]
    fn warning_lines_show_headline_then_remedy_then_the_whole_error_chain() {
        let error = Guidance::new("セッションが切れています", "ログインしてください")
            .wrap(anyhow!("expired token"))
            .context("ListTasks に失敗しました");

        assert_eq!(
            PLAIN.warning_lines("staging を飛ばします", &error),
            [
                "! staging を飛ばします",
                "  ログインしてください",
                "  ListTasks に失敗しました",
                "  セッションが切れています",
                "  expired token",
            ]
        );
    }

    #[test]
    fn display_width_counts_full_width_characters_as_two() {
        assert_eq!(display_width("RUNNING"), 7);
        assert_eq!(display_width("止め忘れ"), 8);
        assert_eq!(display_width("1 時間 3 分"), 11);
        assert_eq!(display_width("─"), 1);
    }

    #[test]
    fn only_changed_status_is_emitted_when_not_spinning() {
        let mut lines = ChangedLines::default();

        assert_eq!(lines.changed("PENDING".into()).as_deref(), Some("PENDING"));
        assert_eq!(lines.changed("PENDING".into()), None);
        assert_eq!(lines.changed("RUNNING".into()).as_deref(), Some("RUNNING"));
    }

    #[test]
    fn spinner_template_is_valid() {
        assert!(ProgressStyle::with_template(SPINNER_TEMPLATE).is_ok());
    }

    #[test]
    fn clock_shows_minutes_and_seconds() {
        assert_eq!(clock(Duration::from_secs(21)), "00:21");
        assert_eq!(clock(Duration::from_millis(125_900)), "02:05");
    }

    #[test]
    fn duration_is_seconds_minutes_or_hours_without_zero_parts() {
        assert_eq!(duration(Duration::from_millis(45_900)), "45 秒");
        assert_eq!(duration(Duration::from_secs(720)), "12 分");
        assert_eq!(duration(Duration::from_secs(125)), "2 分 5 秒");
        assert_eq!(duration(Duration::from_secs(3600 + 59)), "1 時間");
        assert_eq!(
            duration(Duration::from_secs(2 * 3600 + 3 * 60 + 9)),
            "2 時間 3 分"
        );
    }

    #[test]
    fn task_id_is_the_last_segment_of_the_arn_with_or_without_cluster() {
        assert_eq!(
            task_id("arn:aws:ecs:us-east-1:123456789012:task/example-staging/0123456789abcdef"),
            "0123456789abcdef"
        );
        assert_eq!(
            task_id("arn:aws:ecs:us-east-1:123456789012:task/0123456789abcdef"),
            "0123456789abcdef"
        );
    }

    #[test]
    fn short_task_id_is_the_first_8_characters_of_the_id() {
        assert_eq!(
            short_task_id(
                "arn:aws:ecs:us-east-1:123456789012:task/example-staging/0123456789abcdef"
            ),
            "01234567"
        );
        assert_eq!(
            short_task_id("arn:aws:ecs:us-east-1:123456789012:task/0123456789abcdef"),
            "01234567"
        );
        assert_eq!(
            short_task_id("arn:aws:ecs:us-east-1:123456789012:task/c/abc"),
            "abc"
        );
    }

    #[test]
    fn task_definition_name_is_family_and_revision() {
        assert_eq!(
            task_definition_name("arn:aws:ecs:us-east-1:123456789012:task-definition/worker:42"),
            "worker:42"
        );
    }

    #[test]
    fn resource_ids_are_shortened_to_8_hex_characters() {
        assert_eq!(
            short_resource_id("subnet-0123456789abcdef0"),
            "subnet-01234567…"
        );
        assert_eq!(short_resource_id("sg-01234567"), "sg-01234567");
        assert_eq!(short_resource_id("unknown"), "unknown");
    }
}
