# ecsh

Amazon ECS で使い捨てのタスクを起動し、ECS Exec で入り、抜けたらタスクを止める CLI。

> **開発中**: いまは `run` がサービスのネットワーク設定とタスク定義を読んで表示するところまでです。タスクはまだ起動しません。

## 目的

ECS 上のアプリケーションでバッチやコンソールを手で動かすとき、「使い捨てタスクを起動する」「exec できるようになるまで待つ」「入る」「終わったら止める」を別々に行うと、次のことが起きます。

- タスクが RUNNING になっても ExecuteCommandAgent が起動するまでは exec できず、いつ入れるかが分からない
- 抜けたあともタスクが動き続け、止め忘れる
- 同じクラスタで動いている常駐タスクに入ってしまう余地がある

ecsh はこれを 1 コマンドにまとめます。

- ExecuteCommandAgent が RUNNING になるのを待ち、なったら自動で入る
- 入る先は、自分が起動したタスクに固定する
- 抜けたらタスクを止める。ecsh が異常終了したときに備え、コンテナ側にもセッションが無くなったら終了する仕組みを持たせる

## 必要なもの

- Rust（ビルド用）
- AWS の認証情報（通常の AWS SDK と同じ解決順）
- [Session Manager plugin](https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html)
- 対象の ECS サービスで ECS Exec が有効になっていること

## インストール

```sh
cargo install --git https://github.com/r-ishino/ecsh
```

## 設定

`$XDG_CONFIG_HOME/ecsh/config.toml`（未設定なら `~/.config/ecsh/config.toml`）に置きます。`--config <PATH>` で別の場所も指定できます。

[config.example.toml](config.example.toml) を写して値を書き換えてください。

```toml
[profiles.staging]
region = "us-east-1"
cluster = "example-staging"
service = "worker"   # ネットワーク設定のコピー元にするサービス
container = "app"    # exec で入るコンテナ
aws_profile = "example"  # 省略可
```

AWS プロファイルは次の順で決まります。`run` は使ったプロファイルとその出どころを表示します。

1. 環境変数 `AWS_PROFILE`
2. 設定の `aws_profile`
3. どちらも無ければ AWS SDK の既定の解決順

## 使い方

```sh
ecsh run staging   # 使い捨てタスクを起動して入る。抜けたら止める
ecsh ps staging    # ecsh が起動したタスクのうち、動いているものを一覧する
ecsh gc staging    # 残ってしまった ecsh のタスクを止める
```

## 開発

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
