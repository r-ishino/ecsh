# run

`ecsh run -- <コマンド...>` は、`--` より後ろのコマンドをコンテナのコマンドにして、使い捨てタスクを起動する。プロファイルは他のコマンドと同じく最初に選ぶ。

```sh
ecsh run -- bundle exec rake db:migrate:status
ecsh run -- bundle exec rake 'users:import[2026-09-01,dry]'  # [ ] は引用符で囲む（zsh が展開する）
ecsh run -- bundle exec rake users:import LIMIT=10           # rake は KEY=VALUE を環境変数として受け取る
ecsh run -- env LIMIT=10 bin/import                          # rake 以外は env を使う
ecsh run -- sh -c 'bin/prepare && bundle exec rake users:import'
ecsh run -d -- bundle exec rake users:import                 # 起動だけして抜ける
ecsh run -y -d staging -- bundle exec rake users:import      # プロファイル名を付けて確認も省く
```

## 前提

- アカウントでタスクの [新しい ARN とリソース ID の形式](https://docs.aws.amazon.com/AmazonECS/latest/developerguide/ecs-account-settings.html#ecs-resource-ids) が有効になっていること（起動時にタスクへタグを付けるのに要る）
- `ecs:TagResource`・`ecs:DescribeTaskDefinition`・`logs:GetLogEvents` の権限があること

## コマンドの渡し方

- 引数はシェルを通さず、そのまま渡す。コマンドがコンテナのメインプロセスになり、StopTask が送る SIGTERM を受け取る
- コマンドが終わればタスクも止まる。上限時間は無い
- ネットワーク設定などの起動の設定とタスク定義の選び方は、`exec` と同じ（[exec.md](exec.md#起動まで)）。使った AWS プロファイルも同じように出す
- `&&` やパイプなどシェルの構文を使うときは、自分で `sh -c` で包む。この場合はシェルがメインプロセスになり、SIGTERM をコマンドに渡さない。タスクを止めると、コマンドは後始末できないまま、停止のタイムアウトの後に殺される
- 起動前に `exec` と同じく y/N を聞く（`--yes` / `-y` で省く）
- コマンドを付けずに `ecsh run` と打つと、プロファイルも聞かずに `ecsh exec` を案内する。何も起動しない
- タスクの大きさは `exec` と同じく変えられる（[size.md](size.md)）

## 終わるまで待つ（既定）

- コマンドの出力は CloudWatch Logs から読んで stdout に出す。進み具合と結果は stderr に出す
- ロググループとストリームは、タスク定義でコンテナに設定した `awslogs` のログ設定から決める（`awslogs-group` と `awslogs-stream-prefix` が要る）
- タスクが止まったら残りの出力を出し、所要時間を添えて、コマンドの終了コードで終わる。終了コードが無いとき（タスクの起動に失敗したなど）は、タスクが止まった理由を出して 1 で終わる
- コンテナが `awslogs` を使っていなければ、注意を出して、タスクが止まるのを待つだけにする

## --detach

`--detach`（`-d`）は、タスクを起動したら待たずに 0 で終わる。タスク ID を出し、後から出力を読むための `ecsh logs` を案内する。

## シグナル

待っている間は次のとおり。

- Ctrl-C を押すと出力を止め、タスクも止めるかを聞く。`y` ならタスクを止め、止まるのを待って 130 で終わる。`N` か Enter なら、タスクは最後まで動かしたまま 130 で終わる
- 聞いている間にもう一度 Ctrl-C を押すと、タスクは動かしたまま終わる。タスクが止まるのを待っている間の Ctrl-C は、待たずに終わる
- ターミナルを閉じる（SIGHUP）・SIGTERM・stdin がターミナルでないときの Ctrl-C は、タスクを動かしたまま 128 + シグナル番号で終わる

## 履歴

- run は毎回、`ecsh logs` が読む履歴に記録する（[logs.md](logs.md#履歴)）
- 履歴に書けないときは、注意を出してそのまま続ける。この場合、`-d` のときや止めずに抜けたときは、`ecsh logs` の代わりに AWS コンソールを案内する

## ps・gc での見え方

run が起動したタスクも、`exec` と同じく `startedBy = ecsh/$USER` を持つ。加えてタグ `ecsh:mode = run` が付き、`ps` の接続の列に run と出る。`gc` ではチェック無しで並ぶので、暴走したコマンドはそこでチェックして止められる。
