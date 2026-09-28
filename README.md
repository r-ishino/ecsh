# ecsh

A CLI that launches a one-off task on Amazon ECS, drops you into it with ECS Exec, and stops the task when you exit. It can also run a command as a one-off task and stream its output.

Amazon ECS で使い捨てタスクを起動して ECS Exec で入り、抜けたらタスクを止める CLI。コマンドを使い捨てタスクとして流し、出力を見ることもできる。

## Why / なぜ

Doing "launch a one-off task", "wait until exec is available", "get in", and "stop it when done" by hand, you don't know when you can get in (the ExecuteCommandAgent starts after the task is RUNNING), you forget to stop the task, and you might end up in a long-running task in the same cluster. ecsh waits for the agent and gets you in automatically, only ever connects to the task it launched, and stops it when you exit; if ecsh dies, the task stops on its own 12 hours after it starts.

「使い捨てタスクを起動する」「exec できるまで待つ」「入る」「終わったら止める」を手でやると、いつ入れるか分からず（ExecuteCommandAgent はタスクが RUNNING になった後に起動する）、止め忘れ、同じクラスタで常駐しているタスクに入ってしまうこともある。ecsh はエージェントを待って自動で入り、自分が起動したタスクにだけつなぎ、抜けたら止める。ecsh が異常終了しても、タスクは起動から 12 時間で自分で止まる。

## Requirements / 必要なもの

The Rust toolchain to build it (see [Installation](#installation--インストール)), AWS credentials (resolved the same way as the standard AWS SDK), the [Session Manager plugin](https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html) on `PATH`, and the ECS Exec prerequisites (such as the task role's SSM permissions) met for the target service's task definition. `run` needs a few more account settings and permissions; see [docs/run.md](docs/run.md).

ビルドに Rust のツールチェーン（入れ方は[インストール](#installation--インストール)）、AWS の認証情報（AWS SDK の標準と同じ順で解決）、`PATH` 上の Session Manager plugin、対象サービスのタスク定義が ECS Exec の前提（タスクロールの SSM の権限など）を満たしていること。`run` にはアカウントの設定と権限がもう少し要る（[docs/run.md](docs/run.md)）。

## Installation / インストール

If you don't have `cargo`, install Rust first and open a new terminal.

`cargo` が無ければ、先に Rust を入れて新しいターミナルを開く。

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then install ecsh. Run the same command to update. See [docs/install.md](docs/install.md) if it fails.

ecsh を入れる。更新も同じコマンド。うまくいかないときは [docs/install.md](docs/install.md)。

```sh
cargo install --git https://github.com/r-ishino/ecsh --locked
```

## Configuration / 設定

Copy [config.example.toml](config.example.toml) to `$XDG_CONFIG_HOME/ecsh/config.toml` (or `~/.config/ecsh/config.toml`) and edit it; `--config <PATH>` points elsewhere. The AWS profile is `AWS_PROFILE`, then `aws_profile` in the config, then the AWS SDK's default.

[config.example.toml](config.example.toml) を `$XDG_CONFIG_HOME/ecsh/config.toml`（未設定なら `~/.config/ecsh/config.toml`）にコピーして値を直す。`--config <PATH>` で別の場所を指せる。AWS プロファイルは `AWS_PROFILE` → 設定の `aws_profile` → AWS SDK の既定 の順に決まる。

```toml
[profiles.staging]
region = "us-east-1"
cluster = "example-staging"
service = "worker"       # service to copy the network configuration from / ネットワーク設定を写すサービス
container = "app"        # container to exec into / 入るコンテナ
aws_profile = "example"  # optional / 省略可
```

## Usage / 使い方

Each command first lists the profiles in the config (↑↓ and Enter to pick, typing narrows the list, Esc cancels with exit status 1). Details of each command are in Japanese under [docs/](docs/).

各コマンドはまず設定のプロファイルを一覧で出す（↑↓ と Enter で選び、文字を打つと絞り込み、Esc で終了コード 1 でやめる）。各コマンドの細かい挙動は [docs/](docs/) に日本語で書いてある。

```sh
ecsh exec                                   # launch a task, get in, stop it when you exit / 起動して入り、抜けたら止める
ecsh run -- bundle exec rake db:migrate:status  # run a command and stream its output / コマンドを流して出力を見る
ecsh ps                                     # list your running tasks / 動いている自分のタスクを一覧する
ecsh gc                                     # stop tasks left behind / 残ったタスクを選んで止める
ecsh logs                                   # show the output of a past run / 過去の run の出力を見る
ecsh open                                   # open a running task in the AWS console / 動いているタスクを AWS コンソールで開く
ecsh exec --size                            # change the task size for one launch / 1 回だけタスクの大きさを変える
```

`ps --all` and `gc --all` go through every profile, and `logs --last` opens your latest run whatever the profile; these skip the profile list. See [exec](docs/exec.md), [run](docs/run.md), [ps](docs/ps.md), [gc](docs/gc.md), [logs](docs/logs.md), [open](docs/open.md), and [task size](docs/size.md).

`ps --all` と `gc --all` は全プロファイルを回り、`logs --last` はプロファイルを問わず直近の run を開く。これらはプロファイルの一覧を出さない。詳しくは [exec](docs/exec.md)・[run](docs/run.md)・[ps](docs/ps.md)・[gc](docs/gc.md)・[logs](docs/logs.md)・[open](docs/open.md)・[タスクの大きさ](docs/size.md)。

### Passing the profile name / プロファイル名を渡す

Put the profile name right after the command to skip the profile list. This is how to use ecsh when stdin is not a terminal, such as from a script, often with `--yes` (`-y`) to skip the y/N prompt before launching. The name cannot be combined with `--all`.

コマンドの直後にプロファイル名を置くと、プロファイルの一覧を飛ばせる。スクリプトなど stdin がターミナルでないときはこの形で使い、起動前の y/N を省く `--yes`（`-y`）と組み合わせることが多い。名前は `--all` と併用できない。

```sh
ecsh exec staging --cpu 2 --memory 8GB -y
ecsh run -y -d staging -- bundle exec rake users:import
ecsh gc staging --yes
ecsh logs staging --last
```

## Development / 開発

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo audit
```
