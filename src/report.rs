/// `eprintln!` と同じ 1 行を stderr に出す。書けなくても panic しない
///
/// タブを閉じた（SIGHUP）後は端末への書き込みが失敗する。`eprintln!` はそこで panic し、StopTask に進めずタスクが残る
macro_rules! report {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

pub(crate) use report;
