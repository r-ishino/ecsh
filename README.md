# ecsh

A CLI that launches a one-off task on Amazon ECS, drops you into it with ECS Exec, and stops the task when you exit.

## Why

When you run a batch job or a console by hand for an application on ECS, doing "launch a one-off task", "wait until exec is available", "get in", and "stop it when done" as separate steps leads to the following:

- Even after the task is RUNNING, you cannot exec into it until the ExecuteCommandAgent has started, so you don't know when you can get in
- The task keeps running after you exit, and you forget to stop it
- You might end up in a long-running task that happens to be running in the same cluster

ecsh combines these into a single command:

- It waits for the ExecuteCommandAgent to become RUNNING, then gets you in automatically
- It only ever connects to the task it launched
- It stops the task when you exit. In case ecsh terminates abnormally, the task is set to stop on its own 12 hours after it starts

## Requirements

- Rust (to build)
- AWS credentials (resolved the same way as the standard AWS SDK)
- [Session Manager plugin](https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html) on `PATH` (`exec` checks for it before launching anything)
- ECS Exec enabled on the target ECS service

## Installation

```sh
cargo install --git https://github.com/r-ishino/ecsh --locked
```

## Configuration

Put the config at `$XDG_CONFIG_HOME/ecsh/config.toml` (or `~/.config/ecsh/config.toml` if unset). You can point to a different location with `--config <PATH>`.

Copy [config.example.toml](config.example.toml) and edit the values.

```toml
[profiles.staging]
region = "us-east-1"
cluster = "example-staging"
service = "worker"   # service to copy the network configuration from
container = "app"    # container to exec into
aws_profile = "example"  # optional
```

The AWS profile is chosen in the following order. `exec` prints the profile it used and where it came from.

1. The `AWS_PROFILE` environment variable
2. `aws_profile` in the config
3. If neither is set, the AWS SDK's default resolution order

## Usage

```sh
ecsh exec staging  # launch a one-off task and get in; stop it when you exit
ecsh ps staging    # list the tasks launched by ecsh that are still running
ecsh ps --all      # the same, across every profile in the config
ecsh gc staging    # stop leftover ecsh tasks
```

`exec` starts `/bin/sh` in the container, with the profile name in the prompt (`[staging] /app # `). When you exit the shell, ecsh stops the task. Even if the session ends abnormally (for example, the connection drops), ecsh still stops the task and exits with 0 as long as stopping succeeds; it only prints the session's exit status.

`exec` prints its progress to stderr (in Japanese). It looks roughly like this:

```
  staging  →  example-staging / worker / app
  AWS  example（設定の aws_profile）· us-east-1
  ネットワーク  subnet-01234567… · sg-01234567… · パブリック IP なし

✓ タスクを起動しました  0123abcd（worker:42）
  12 時間後に自動で止まります · startedBy ecsh/alice
✓ 入れるようになりました（45 秒）
  exit で抜けるとタスクを止めます

[staging] /app # exit
✓ タスクを止めました  0123abcd（入っていた時間 12 分）
```

The task is shown by the first 8 characters of its ID and the task definition by `family:revision`; the full task ARN is printed only when you may need to stop the task yourself. While waiting to get in, a single spinner line shows the elapsed time and the task / agent status. Errors are shown as a one-line summary marked with `✗`, followed by what to do (if any) and the underlying causes. If an AWS call fails because the AWS SSO session has expired, ecsh tells you to run `aws sso login` with the AWS profile it resolved.

When stderr is not a terminal, or the `NO_COLOR` environment variable is set, ecsh prints no colors or spinner; while waiting, it prints one line each time the status changes.

Signals after the task has been launched:

- Ctrl-C, closing the terminal (SIGHUP), or SIGTERM while waiting to get in stops the task, then exits
- Ctrl-C during the session goes to the command running in the container and does not end ecsh. Closing the terminal or SIGTERM ends the session, stops the task, then exits
- Pressing Ctrl-C again while ecsh is stopping the task after a signal exits without waiting. The task may be left running; stop it with `ecsh gc`

Before the task is launched, Ctrl-C simply exits.

If you omit the profile name, the profiles in the config are shown as a list. Pick one with ↑↓ and Enter (Esc to cancel). The list cannot be shown when stdin is not a terminal, so pass the name in that case.

`exec` always asks y/N before launching the task, for every profile. Anything other than `y` or `yes` cancels the launch. `--yes` (`-y`) skips the prompt. When stdin is not a terminal and `--yes` is not given, it fails with an error instead of launching.

While `exec` is in a task, it holds an exclusive lock on `sessions/<task ID>.lock` under the state directory (`$XDG_STATE_HOME/ecsh`, or `~/.local/state/ecsh` when `XDG_STATE_HOME` is unset), and removes the file when it exits. This marks the task as in use from this machine. If the file cannot be created, `exec` prints a warning and carries on.

`ps` lists your tasks (`startedBy = ecsh/$USER`) in the profile's cluster that have not been told to stop. The table goes to stdout, so you can pipe it; notes and warnings go to stderr. It looks roughly like this:

```
ID        接続      状態     起動から     タスク定義  自動停止まで
0123abcd  接続中    RUNNING  12 分        worker:42   11 時間 48 分
89abcdef  止め忘れ  RUNNING  2 時間 5 分  worker:42   9 時間 55 分
```

- 接続 (connection) is 接続中 when `exec` on this machine is in the task, 止め忘れ (left behind) when its lock file remains but no `exec` holds it, 不明 (unknown) when there is no lock file (launched from another machine or by an older ecsh), and run for tasks that run a command and are not meant to be connected to
- 起動から is the time since the task was launched; 自動停止まで is the time left until the 12-hour limit, counted from when the task started (─ while it is pending, and for run tasks)
- Left-behind tasks are shown in yellow, followed by a hint to stop them with `ecsh gc`. For unknown tasks, 起動から turns yellow after 1 hour and red after 2 hours
- With no tasks, `ps` prints a line to stderr and exits with 0

`ps --all` goes through every profile in parallel and adds a profile column. Profiles that point to the same region, cluster, and AWS profile are asked only once, under the name that comes first. A profile that fails (for example, an expired SSO session) is reported on stderr and skipped; `ps --all` fails only when every profile fails. When every profile succeeds, `ps --all` also removes lock files that no `exec` holds and whose tasks are no longer running.

`ecsh run` no longer gets you into a task; it launches nothing and points you to `ecsh exec`. It is reserved for a future subcommand that runs a command as the task's command.

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
