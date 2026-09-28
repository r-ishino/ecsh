# インストール

ecsh はビルド済みのバイナリを配っていないので、`cargo install` でソースからビルドして入れる。手順は [README](../README.md#installation--インストール) にあり、ここにはその補足を書く。

## Rust を入れる

- rustup のインストーラに聞かれたら、既定のまま進めてよい
- インストーラは Rust のツールチェーンを `~/.rustup` と `~/.cargo` に入れ、`~/.cargo/bin` を `PATH` に足す設定をシェルの設定ファイルに書く。管理者権限は要らない
- `PATH` の設定が効くのは、新しく開いたターミナルから。今のターミナルで続けるなら `. "$HOME/.cargo/env"` を打つ
- `cargo --version` がバージョンを出せば入っている
- ビルドが ``linker `cc` not found`` で止まったら、`xcode-select --install` を打ってからもう一度 `cargo install` する

## ecsh を入れる

- `--locked` を付けると、repo の `Cargo.lock` にある版で依存をビルドする。外すと依存の新しい版を拾い、手元で確かめていない組み合わせになる
- 入る先は `~/.cargo/bin/ecsh`
- 初回は依存（AWS SDK など）のビルドに数分かかる
- 入ったかは `ecsh --version` で確かめる

## 更新する

同じコマンドをもう一度打つ。

```sh
cargo install --git https://github.com/r-ishino/ecsh --locked
```

- cargo は入れた commit を覚えていて、`main` の先頭と比べる。commit が変わっていれば、`Cargo.toml` のバージョンが `0.1.0` のままでもビルドし直して入れ替える。このとき `Replaced package ecsh v0.1.0 (...#<古い commit>) with ecsh v0.1.0 (...#<新しい commit>)` と出る
- 最新なら何もしない。`Ignored package ecsh v0.1.0 (...) is already installed, use --force to override` と出るが、`--force` を付ける必要はない
- 入っている commit は `cargo install --list` の `ecsh v0.1.0 (https://github.com/r-ishino/ecsh#<commit>)` で見られる

## 消す

```sh
cargo uninstall ecsh
```

`~/.cargo/bin/ecsh` が消える。設定ファイル（`~/.config/ecsh/`）と状態ディレクトリ（`~/.local/state/ecsh/`、run の履歴など）は消えないので、要らなければ手で消す。Rust ごと消すなら `rustup self uninstall`。
