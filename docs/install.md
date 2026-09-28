# インストール

ecsh はビルド済みのバイナリを配っていないので、`cargo install` でソースからビルドして入れる。手順そのものは [README](../README.md#installation--インストール) にあり、ここにはその補足を書く。

## Rust を入れる

- ビルドが ``linker `cc` not found`` などリンカの無いエラーで止まったら、`xcode-select --install` で Xcode Command Line Tools を入れる。入っているかは `xcode-select -p` がパスを出すかで分かる
- rustup のインストーラに聞かれたら、既定のまま進めてよい
- rustup のインストーラは Rust のツールチェーンを `~/.rustup` と `~/.cargo` に入れ、`~/.cargo/bin` を `PATH` に足す設定をシェルの設定ファイルに書く。管理者権限は要らない
- 設定が効くのは新しく開いたターミナルから。開き直さずに今のターミナルで続けるなら `. "$HOME/.cargo/env"` を打つ
- `cargo --version` がバージョンを出せば入っている

## ecsh を入れる

- `--locked` は repo の `Cargo.lock` の版で依存をビルドする。外すと依存の新しい版を拾い、手元で確かめていない組み合わせになる
- 入る先は `~/.cargo/bin/ecsh`
- 初回は依存（AWS SDK など）のビルドに数分かかる
- 入ったかは `ecsh --version` で確かめる

## 更新する

同じコマンドをもう一度打つ。

```sh
cargo install --git https://github.com/r-ishino/ecsh --locked
```

- cargo は入れた commit を覚えていて、`main` の先頭と比べる。`Cargo.toml` のバージョンが `0.1.0` のままでも、commit が変わっていればビルドし直して入れ替わる。このとき `Replaced package ecsh v0.1.0 (...#<古い commit>) with ecsh v0.1.0 (...#<新しい commit>)` と出る
- 最新なら何もしない。`Ignored package ecsh v0.1.0 (...) is already installed, use --force to override` と出るが、`--force` は要らない
- 入っている commit は `cargo install --list` の `ecsh v0.1.0 (https://github.com/r-ishino/ecsh#<commit>)` で見られる

## 消す

```sh
cargo uninstall ecsh
```

`~/.cargo/bin/ecsh` が消える。設定ファイル（`~/.config/ecsh/`）と状態ディレクトリ（`~/.local/state/ecsh/`、run の履歴など）は消えないので、要らなければ手で消す。Rust ごと消すなら `rustup self uninstall`。
