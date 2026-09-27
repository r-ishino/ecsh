//! この Mac の ecsh が入っているタスクを、`<state_dir>/sessions/<タスク ID>.lock` の排他ロック（flock）で示す
//!
//! ロックはプロセスが終われば kill -9 でも OS が外すので、ファイルが残っていても持ち主がいなければ止め忘れと分かる

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::xdg;

pub fn sessions_dir() -> Result<PathBuf> {
    xdg::state_dir()
        .map(|dir| dir.join("sessions"))
        .ok_or_else(|| anyhow!("XDG_STATE_HOME も HOME も未設定です"))
}

fn lock_path(sessions_dir: &Path, task_id: &str) -> PathBuf {
    sessions_dir.join(format!("{task_id}.lock"))
}

/// 接続中の印。持っている間ロックを保ち、落とすとファイルを消す
#[derive(Debug)]
pub struct SessionLock {
    path: PathBuf,
    _file: File,
}

impl SessionLock {
    pub fn acquire(sessions_dir: &Path, task_id: &str) -> Result<Self> {
        fs::create_dir_all(sessions_dir)
            .with_context(|| format!("ディレクトリを作れません: {}", sessions_dir.display()))?;
        let path = lock_path(sessions_dir, task_id);
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("ロックファイルを作れません: {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { path, _file: file }),
            // 持ち主のいるファイルは消さない。消すと持ち主が接続中なのに ps / gc から見えなくなる
            Err(TryLockError::WouldBlock) => Err(anyhow!(
                "ロックをほかのプロセスが持っています: {}",
                path.display()
            )),
            Err(TryLockError::Error(error)) => {
                let _ = fs::remove_file(&path);
                Err(anyhow::Error::new(error)
                    .context(format!("ロックを取れません: {}", path.display())))
            }
        }
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        // ロックを外す（ファイルを閉じる）より先に消す。逆だと、外れた瞬間に ps / gc が止め忘れと判定しうる
        let _ = fs::remove_file(&self.path);
    }
}

/// タスクにこの Mac の ecsh が入っているか
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    /// ロックをほかのプロセスが持っている
    Connected,
    /// ロックファイルはあるが持ち主がいない
    Abandoned,
    /// ロックファイルが無い。別の Mac から起動したか、ロックファイルを作る前の版で起動した
    Unknown,
}

pub fn connection(sessions_dir: &Path, task_id: &str) -> Result<Connection> {
    probe(&lock_path(sessions_dir, task_id), |_| Ok(()))
}

/// 持ち主のいないロックファイルを消す。返すのは消す前の状態で、消したのは `Abandoned` のときだけ
pub fn remove_if_abandoned(sessions_dir: &Path, task_id: &str) -> Result<Connection> {
    probe(&lock_path(sessions_dir, task_id), |path| {
        match fs::remove_file(path) {
            // 判定の間に持ち主の exec が抜けて消した
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            removed => removed,
        }
        .with_context(|| format!("ロックファイルを消せません: {}", path.display()))
    })
}

/// ロックファイルのあるタスクの ID。ディレクトリがまだ無ければ空
pub fn task_ids(sessions_dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(sessions_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(anyhow::Error::new(error).context(format!(
                "ディレクトリを読めません: {}",
                sessions_dir.display()
            )));
        }
    };
    let mut ids = Vec::new();
    for entry in entries {
        let path = entry
            .with_context(|| format!("ディレクトリを読めません: {}", sessions_dir.display()))?
            .path();
        if path
            .extension()
            .is_some_and(|extension| extension == "lock")
            && let Some(id) = path.file_stem().and_then(|stem| stem.to_str())
        {
            ids.push(id.to_owned());
        }
    }
    ids.sort();
    Ok(ids)
}

/// ロックを取れるか試す。取れたら、持ったまま `on_abandoned` を呼んでから外す
fn probe(path: &Path, on_abandoned: impl FnOnce(&Path) -> Result<()>) -> Result<Connection> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Connection::Unknown),
        Err(error) => {
            return Err(anyhow::Error::new(error)
                .context(format!("ロックファイルを開けません: {}", path.display())));
        }
    };
    match file.try_lock() {
        Ok(()) => {
            on_abandoned(path)?;
            Ok(Connection::Abandoned)
        }
        Err(TryLockError::WouldBlock) => Ok(Connection::Connected),
        Err(TryLockError::Error(error)) => Err(anyhow::Error::new(error)
            .context(format!("ロックを確かめられません: {}", path.display()))),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const TASK_ID: &str = "0123456789abcdef0123456789abcdef";

    /// テストごとに別の空ディレクトリ。消すのは OS の一時ディレクトリの掃除に任せる
    fn temp_sessions_dir() -> PathBuf {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ecsh-session-lock-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        dir.join("sessions")
    }

    /// 別のプロセスが持っているロックの代わり。flock はファイルを開くごとに別のロックになる
    fn hold_lock_elsewhere(path: &Path) -> File {
        let file = File::open(path).unwrap();
        file.try_lock().unwrap();
        file
    }

    fn leave_abandoned_lock_file(dir: &Path) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = lock_path(dir, TASK_ID);
        File::create(&path).unwrap();
        path
    }

    #[test]
    fn acquiring_creates_the_directory_and_a_lock_file_named_after_the_task_id() {
        let dir = temp_sessions_dir();

        let _lock = SessionLock::acquire(&dir, TASK_ID).unwrap();

        assert!(dir.join(format!("{TASK_ID}.lock")).is_file());
    }

    #[test]
    fn task_is_connected_while_the_lock_is_held() {
        let dir = temp_sessions_dir();
        let _lock = SessionLock::acquire(&dir, TASK_ID).unwrap();

        assert_eq!(connection(&dir, TASK_ID).unwrap(), Connection::Connected);
    }

    #[test]
    fn dropping_the_lock_removes_the_lock_file() {
        let dir = temp_sessions_dir();
        let lock = SessionLock::acquire(&dir, TASK_ID).unwrap();

        drop(lock);

        assert!(!lock_path(&dir, TASK_ID).exists());
        assert_eq!(connection(&dir, TASK_ID).unwrap(), Connection::Unknown);
    }

    #[test]
    fn lock_file_without_an_owner_is_abandoned() {
        let dir = temp_sessions_dir();
        leave_abandoned_lock_file(&dir);

        assert_eq!(connection(&dir, TASK_ID).unwrap(), Connection::Abandoned);
    }

    #[test]
    fn task_without_a_lock_file_is_unknown() {
        let dir = temp_sessions_dir();

        assert_eq!(connection(&dir, TASK_ID).unwrap(), Connection::Unknown);
    }

    #[test]
    fn checking_the_connection_does_not_take_the_lock_away() {
        let dir = temp_sessions_dir();
        let path = leave_abandoned_lock_file(&dir);

        connection(&dir, TASK_ID).unwrap();

        let _held = hold_lock_elsewhere(&path);
        assert_eq!(connection(&dir, TASK_ID).unwrap(), Connection::Connected);
    }

    #[test]
    fn acquiring_fails_without_removing_a_lock_file_held_by_someone_else() {
        let dir = temp_sessions_dir();
        let path = leave_abandoned_lock_file(&dir);
        let _held = hold_lock_elsewhere(&path);

        let error = SessionLock::acquire(&dir, TASK_ID).unwrap_err();

        assert!(error.to_string().contains("ほかのプロセス"), "{error:#}");
        assert_eq!(connection(&dir, TASK_ID).unwrap(), Connection::Connected);
    }

    #[test]
    fn abandoned_lock_file_is_removed() {
        let dir = temp_sessions_dir();
        let path = leave_abandoned_lock_file(&dir);

        assert_eq!(
            remove_if_abandoned(&dir, TASK_ID).unwrap(),
            Connection::Abandoned
        );
        assert!(!path.exists());
    }

    #[test]
    fn lock_file_of_a_connected_task_is_kept() {
        let dir = temp_sessions_dir();
        let _lock = SessionLock::acquire(&dir, TASK_ID).unwrap();

        assert_eq!(
            remove_if_abandoned(&dir, TASK_ID).unwrap(),
            Connection::Connected
        );
        assert!(lock_path(&dir, TASK_ID).exists());
    }

    #[test]
    fn removing_only_touches_the_given_task() {
        let dir = temp_sessions_dir();
        leave_abandoned_lock_file(&dir);
        let other = "fedcba9876543210fedcba9876543210";

        assert_eq!(
            remove_if_abandoned(&dir, other).unwrap(),
            Connection::Unknown
        );
        assert!(lock_path(&dir, TASK_ID).exists());
    }

    #[test]
    fn task_ids_are_the_names_of_lock_files() {
        let dir = temp_sessions_dir();
        leave_abandoned_lock_file(&dir);
        let _lock = SessionLock::acquire(&dir, "fedcba98").unwrap();
        File::create(dir.join("notes.txt")).unwrap();

        assert_eq!(
            task_ids(&dir).unwrap(),
            ["0123456789abcdef0123456789abcdef", "fedcba98"]
        );
    }

    #[test]
    fn no_task_ids_before_the_directory_exists() {
        assert!(task_ids(&temp_sessions_dir()).unwrap().is_empty());
    }
}
