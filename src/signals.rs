use std::fmt;
use std::future::Future;

use anyhow::{Context, Result};
use tokio::signal::unix::{self, SignalKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// Ctrl-C
    Interrupt,
    /// タブを閉じた
    Hangup,
    Terminate,
}

impl Signal {
    fn number(self) -> u8 {
        match self {
            Self::Hangup => 1,
            Self::Interrupt => 2,
            Self::Terminate => 15,
        }
    }

    /// シェルの慣習どおり 128 + シグナル番号
    pub fn exit_code(self) -> u8 {
        128 + self.number()
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Interrupt => "SIGINT",
            Self::Hangup => "SIGHUP",
            Self::Terminate => "SIGTERM",
        })
    }
}

/// タスクを起動した後、シグナルを受けた時点で exec が何をしているか
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// RunTask から、Agent を待ってセッションを始めるまで
    Preparing,
    /// session-manager-plugin が動いている間
    InSession,
    /// 抜けた後や、エラーの後始末の StopTask
    Stopping,
    /// シグナルを受けて StopTask している間
    StoppingOnSignal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 受け流して、今の処理を待ち続ける
    Ignore,
    /// 今の処理をやめ、タスクを止めてから終了する
    StopTask,
    /// タスクを止めるのを待たずに終了する
    ExitNow,
}

pub fn action(stage: Stage, signal: Signal) -> Action {
    match (stage, signal) {
        (Stage::Preparing, _) => Action::StopTask,
        (Stage::InSession, Signal::Interrupt) => Action::Ignore,
        (Stage::InSession, Signal::Hangup | Signal::Terminate) => Action::StopTask,
        (Stage::StoppingOnSignal, Signal::Interrupt) => Action::ExitNow,
        (Stage::StoppingOnSignal, Signal::Hangup | Signal::Terminate) => Action::Ignore,
        (Stage::Stopping, _) => Action::Ignore,
    }
}

/// 受け流さなかったシグナル。action は StopTask か ExitNow
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interruption {
    pub signal: Signal,
    pub action: Action,
}

impl fmt::Display for Interruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} を受けて中断しました", self.signal)
    }
}

impl std::error::Error for Interruption {}

/// SIGINT・SIGHUP・SIGTERM の受け口
pub struct Signals {
    interrupt: unix::Signal,
    hangup: unix::Signal,
    terminate: unix::Signal,
}

impl Signals {
    /// 登録した時点から、この 3 つのシグナルではプロセスが終了しなくなる
    pub fn listen() -> Result<Self> {
        let listen =
            |kind: SignalKind| unix::signal(kind).context("シグナルの受け口を登録できません");
        Ok(Self {
            interrupt: listen(SignalKind::interrupt())?,
            hangup: listen(SignalKind::hangup())?,
            terminate: listen(SignalKind::terminate())?,
        })
    }

    /// future の完了を待つ。stage で受け流さないシグナルが先に来たら、future を捨てて Err を返す
    pub async fn watch<F: Future>(
        &mut self,
        stage: Stage,
        future: F,
    ) -> Result<F::Output, Interruption> {
        tokio::select! {
            biased;
            interruption = self.next_interruption(stage) => Err(interruption),
            output = future => Ok(output),
        }
    }

    async fn next_interruption(&mut self, stage: Stage) -> Interruption {
        loop {
            let signal = self.recv().await;
            match action(stage, signal) {
                Action::Ignore => {}
                action => return Interruption { signal, action },
            }
        }
    }

    async fn recv(&mut self) -> Signal {
        tokio::select! {
            _ = self.interrupt.recv() => Signal::Interrupt,
            _ = self.hangup.recv() => Signal::Hangup,
            _ = self.terminate.recv() => Signal::Terminate,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Duration;

    use super::*;

    const ALL: [Signal; 3] = [Signal::Interrupt, Signal::Hangup, Signal::Terminate];

    #[test]
    fn any_signal_while_preparing_stops_the_task_before_exiting() {
        for signal in ALL {
            assert_eq!(
                action(Stage::Preparing, signal),
                Action::StopTask,
                "{signal}"
            );
        }
    }

    #[test]
    fn ctrl_c_in_session_is_left_to_the_session_and_does_not_end_ecsh() {
        assert_eq!(action(Stage::InSession, Signal::Interrupt), Action::Ignore);
    }

    #[test]
    fn closing_the_tab_or_sigterm_in_session_stops_the_task_before_exiting() {
        for signal in [Signal::Hangup, Signal::Terminate] {
            assert_eq!(
                action(Stage::InSession, signal),
                Action::StopTask,
                "{signal}"
            );
        }
    }

    #[test]
    fn any_signal_while_stopping_after_the_session_waits_for_the_stop() {
        for signal in ALL {
            assert_eq!(action(Stage::Stopping, signal), Action::Ignore, "{signal}");
        }
    }

    #[test]
    fn second_ctrl_c_while_stopping_on_a_signal_exits_without_waiting() {
        assert_eq!(
            action(Stage::StoppingOnSignal, Signal::Interrupt),
            Action::ExitNow
        );
    }

    #[test]
    fn closing_the_tab_or_sigterm_while_stopping_on_a_signal_waits_for_the_stop() {
        for signal in [Signal::Hangup, Signal::Terminate] {
            assert_eq!(
                action(Stage::StoppingOnSignal, signal),
                Action::Ignore,
                "{signal}"
            );
        }
    }

    #[test]
    fn exit_code_is_128_plus_the_signal_number() {
        assert_eq!(Signal::Hangup.exit_code(), 129);
        assert_eq!(Signal::Interrupt.exit_code(), 130);
        assert_eq!(Signal::Terminate.exit_code(), 143);
    }

    #[tokio::test]
    async fn sigterm_received_in_session_abandons_the_wait_to_stop_the_task() {
        let mut signals = Signals::listen().unwrap();
        let status = Command::new("kill")
            .args(["-TERM", &std::process::id().to_string()])
            .status()
            .unwrap();
        assert!(status.success());

        let watched = tokio::time::timeout(
            Duration::from_secs(10),
            signals.watch(Stage::InSession, std::future::pending::<()>()),
        )
        .await
        .expect("SIGTERM が届かない");

        assert_eq!(
            watched,
            Err(Interruption {
                signal: Signal::Terminate,
                action: Action::StopTask,
            })
        );
    }
}
