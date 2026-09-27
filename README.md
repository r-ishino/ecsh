# ecsh

A CLI that launches a one-off task on Amazon ECS, drops you into it with ECS Exec, and stops the task when you exit. It can also run a command as a one-off task and stream its output.

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
- For `run`: the [new ARN and resource ID format](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/ecs-account-settings.html#ecs-resource-ids) for tasks enabled in the account (needed to tag a task at launch), and permission for `ecs:TagResource`, `ecs:DescribeTaskDefinition`, and `logs:GetLogEvents`

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
ecsh run staging -- bundle exec rake db:migrate:status  # run a command as a one-off task and stream its output
ecsh ps staging    # list the tasks launched by ecsh that are still running
ecsh ps --all      # the same, across every profile in the config
ecsh gc staging    # choose leftover ecsh tasks and stop them
ecsh gc --all      # the same, across every profile in the config
ecsh logs          # choose a profile, then one of your past runs, and show its output
ecsh logs --last   # show the output of your latest run, whatever the profile
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
ID        接続      状態     起動から     タスク定義  CPU     メモリ  自動停止まで
0123abcd  接続中    RUNNING  12 分        worker:42   1 vCPU  2 GB    11 時間 48 分
89abcdef  止め忘れ  RUNNING  2 時間 5 分  worker:42   2 vCPU  8 GB    9 時間 55 分
```

- 接続 (connection) is 接続中 when `exec` on this machine is in the task, 止め忘れ (left behind) when its lock file remains but no `exec` holds it, 不明 (unknown) when there is no lock file (launched from another machine or by an older ecsh), and run for tasks that run a command and are not meant to be connected to
- CPU and メモリ (memory) are the task's size, including a size overridden at launch (see [Task size](#task-size))
- 起動から is the time since the task was launched; 自動停止まで is the time left until the 12-hour limit, counted from when the task started (─ while it is pending, and for run tasks)
- Left-behind tasks are shown in yellow, followed by a hint to stop them with `ecsh gc`. For unknown tasks, 起動から turns yellow after 1 hour and red after 2 hours
- With no tasks, `ps` prints a line to stderr and exits with 0

`ps --all` goes through every profile in parallel and adds a profile column. Profiles that point to the same region, cluster, and AWS profile are asked only once, under the name that comes first. A profile that fails (for example, an expired SSO session) is reported on stderr and skipped; `ps --all` fails only when every profile fails. When every profile succeeds, `ps --all` also removes lock files that no `exec` holds and whose tasks are no longer running.

`gc` stops the tasks ecsh left behind, for example after ecsh was killed with `kill -9`, the machine lost power, or StopTask failed (they stop on their own after 12 hours anyway). It looks at the same tasks as `ps` and shows them as a checklist on stderr:

- 止め忘れ (left behind) and 不明 (unknown) tasks are checked from the start; run tasks are listed unchecked, so they are stopped only when you check them yourself
- Tasks that `exec` on this machine is in (接続中) are not listed; `gc` prints how many were left out
- Each item shows the task ID, the connection, the time since launch, and the task definition. Toggle items with Space and press Enter to stop the checked ones. Pressing Enter with nothing checked, or Esc, cancels with exit status 1
- With no tasks to list, `gc` says so and exits with 0

`gc` stops the tasks one by one with the reason `ecsh gc`, without waiting for them to reach STOPPED, and prints `✓` or `✗` for each. If some of them fail, it still tries the rest and exits with 1 at the end. It also removes the lock files of the tasks it stopped.

`--yes` (`-y`) skips the checklist and stops the left-behind and unknown tasks; it never stops run tasks. When stdin is not a terminal and `--yes` is not given, `gc` fails with an error without stopping anything.

`gc --all` goes through every profile the same way as `ps --all` and puts all tasks in one checklist, with the profile name at the start of each item. `gc --all --yes` works too, which suits periodic cleanup: it leaves tasks in use and run tasks alone. As with `ps --all`, when every profile succeeds it also removes lock files that no `exec` holds and whose tasks are no longer running.

All of `gc`'s output goes to stderr.

### run

`ecsh run [profile] -- <command...>` launches a one-off task whose container command is the command after `--`. The arguments are passed as they are, without a shell, so the command becomes the container's main process and receives the SIGTERM that StopTask sends. The task stops by itself when the command ends; there is no time limit. The network configuration and the other launch settings are copied from the service, the same as `exec`.

```sh
ecsh run staging -- bundle exec rake db:migrate:status
ecsh run staging -- bundle exec rake 'users:import[2026-09-01,dry]'  # quote [ ] (zsh expands them)
ecsh run staging -- bundle exec rake users:import LIMIT=10            # rake takes KEY=VALUE as environment variables
ecsh run staging -- env LIMIT=10 bin/import                           # otherwise, use env
ecsh run staging -- sh -c 'bin/prepare && bundle exec rake users:import'
ecsh run -d staging -- bundle exec rake users:import                  # launch it and leave
```

When you need `&&`, pipes, or other shell syntax, wrap the command in `sh -c` yourself. In that case the shell is the main process and does not pass SIGTERM on to the command, so stopping the task kills the command without letting it clean up (after the stop timeout).

`run` asks y/N before launching, the same as `exec` (`--yes` / `-y` skips it). Running `ecsh run staging` without a command launches nothing and points you to `ecsh exec staging`.

By default, `run` waits for the command to finish:

- The command's output goes to stdout, read from CloudWatch Logs. ecsh finds the log group and stream from the container's `awslogs` log configuration in the task definition (`awslogs-group` and `awslogs-stream-prefix` are needed). Progress and results go to stderr
- When the task stops, ecsh prints the rest of the output and exits with the command's exit code, along with the time taken. If the command has no exit code (for example, the task failed to start), ecsh prints why the task stopped and exits with 1
- If the container does not use `awslogs`, ecsh prints a warning and only waits for the task to stop

`--detach` (`-d`) launches the task and exits with 0, printing the task ID and pointing you to `ecsh logs` to read the output later.

Signals while `run` is waiting:

- Ctrl-C stops the output and asks whether to stop the task. `y` stops the task, waits until it has stopped, and exits with 130. `N` or Enter leaves the task running to the end and exits with 130
- Ctrl-C again while it is asking leaves the task running. Ctrl-C while ecsh is waiting for the task to stop exits without waiting
- Closing the terminal (SIGHUP), SIGTERM, and Ctrl-C when stdin is not a terminal leave the task running and exit with 128 + the signal number

Each `run` is recorded in the history that `ecsh logs` reads (see below). If the history cannot be written, `run` prints a warning and carries on; `-d` and leaving without stopping then point you to the AWS console instead of `ecsh logs`.

A task launched by `run` has `startedBy = ecsh/$USER` like `exec`, and the tag `ecsh:mode = run`. `ps` shows it as run, and `gc` lists it unchecked, so a runaway command can be stopped by checking it there.

### logs

`ecsh logs` shows the output of a command you ran with `run` on this machine.

```sh
ecsh logs         # choose a profile, then a run from its history
ecsh logs --last  # open the latest run right away, whatever the profile
```

Without a profile name, `logs` first asks for a profile, the same as the other commands, then lists that profile's runs, newest first, with the launch time, the result (終了 <exit code>, or 未確認 when ecsh has not seen the task stop), the time taken, and the command. Pick one with ↑↓ and Enter (Esc cancels with exit status 1). Passing the name (`ecsh logs staging`) skips the profile list; `ecsh logs staging --last` opens the latest run of that profile. The list cannot be shown when stdin is not a terminal; use `--last` in that case. With no runs to show, `logs` says so and exits with 0.

- A run that has finished: its whole output goes to stdout, followed by its exit code and time taken on stderr
- A run that is still going: the output is streamed the same way as when `run` waits, and the exit code is shown when the task stops. Ctrl-C (or closing the terminal, or SIGTERM) only stops watching; the task keeps running, and `logs` exits with 128 + the signal number
- A run launched with `-d` or left without stopping has no exit code in the history yet. `logs` asks ECS (DescribeTasks) and records the result when the task has stopped. ECS forgets stopped tasks after a while; the exit code is then shown as unknown
- If the log stream no longer exists (for example, the log group's retention period has passed), `logs` says the output is gone

`logs` exits with 0 once it has shown the run, whatever the command's exit code.

The history lives in `history.jsonl` under the state directory (the same directory as `sessions/`). `run` adds a line each time it launches a task and records the exit code and time taken once it sees the task stop. Only the latest 100 runs are kept. `exec` is not recorded, since its task only runs `sleep`. Only runs launched from this machine are listed.

### Task size

By default, `exec` and `run` launch the task with the CPU and memory of the task definition. To change them for one launch, pick a size from a list:

```sh
ecsh exec --size   # choose the profile, then the size
ecsh run --size -- bundle exec rake users:import
```

The list starts with "タスク定義のまま" (keep the task definition's size, shown with its current value), so Enter launches without changing anything. The other entries are 0.5 vCPU / 1 GB, 1 vCPU / 2 GB, 1 vCPU / 4 GB, 2 vCPU / 4 GB, 2 vCPU / 8 GB, 4 vCPU / 8 GB, and 4 vCPU / 16 GB. The list is built in and cannot be changed in the config.

For a size that is not on the list, give it directly with `--cpu` (in vCPU, such as `2` or `0.5`) and `--memory` (with a unit, such as `8GB` or `512MB`; 1 GB = 1024 MiB):

```sh
ecsh exec --cpu 2 --memory 8GB
ecsh run --memory 4GB -- bundle exec rake users:import   # the CPU stays as in the task definition
```

If you give only one of them, the other comes from the task definition. The combination is checked against the CPU and memory values Fargate supports before the task is launched, and a combination Fargate does not support is rejected with the allowed memory range for that CPU. Naming the profile works the same way (`ecsh exec staging --cpu 2 --memory 8GB`), and `--cpu` / `--memory` can be combined with `-y`.

`--size` shows a list, so it cannot be combined with `-y`; it cannot be combined with `--cpu` / `--memory` either. It also needs stdin to be a terminal.

The chosen size is shown before the y/N prompt and in the prompt itself. If it is the same as the task definition, nothing is overridden. Changing the size needs permission for `ecs:DescribeTaskDefinition`.

If the task definition sets `memory` (the hard limit), `memoryReservation`, or `cpu` on the container in the profile, ecsh adjusts those values of that container to fit the new task size: the hard limit and `cpu` become the task's value minus what the other containers in the task (sidecars) set, and `memoryReservation` is lowered only when it no longer fits. Without this, a larger task would still be capped by the container's old hard limit, and a smaller task would be rejected because the containers would not fit. The other containers are left as they are. If the sidecars alone do not fit in the new size, ecsh fails before launching the task.

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
