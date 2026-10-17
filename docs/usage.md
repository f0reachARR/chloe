# 使い方

chloe に SSH で入って使う人のための説明である。
サーバーを用意する手順は [インストール](install.md) にある。

## 接続する

GitHub のログイン名をユーザー名にして、GitHub に登録した公開鍵で入る。

```sh
ssh alice@chloe.example.com
```

ユーザー名の `+` のあとに、入るワークスペースを書ける。
書かなければ、自分の個人ワークスペース `default` に入る（なければ作る）。

| ユーザー名 | 入るワークスペース |
| --- | --- |
| `alice` | alice の個人ワークスペース `default` |
| `alice+api` | alice の個人ワークスペース `api` |
| `alice+acme/web` | Organization acme の共有ワークスペース `web` |
| `alice+acme/dev/tools` | acme の team dev の共有ワークスペース `tools` |

`default` 以外のワークスペースは、先に `chloe ws create` で作る。
コマンドを渡すと、そのワークスペースで実行して終わる（`ssh alice@chloe.example.com 'git status'`）。

サーバーに GitHub App を設定していれば、公開鍵を GitHub に登録していなくても入れる。
SSH がコードを表示するので、`https://github.com/login/device` で入力して承認し、Enter を押す。

止まっていたワークスペースは、接続したときに起動する（数秒かかる）。

## VM の中

ワークスペースごとに一つの VM がある。

- **OS と利用者**：Ubuntu 26.04。利用者は `user` で、`sudo` を使える。
- **入っているもの**：Claude Code、Codex、Docker、git、Python 3、curl、tmux など。
- **残るもの**：`/workspace` と、ホームディレクトリ（`/home/user`）は、ワークスペースのディスクにあり、VM を止めても残る。Docker のイメージとコンテナのデータも、別のディスクに残る。
- **残らないもの**：ルートファイルシステムへの変更（`apt install` で入れたものなど）は、既定では VM を止めると消える。残したいときは、ワークスペースのルートの扱いを `persistent` にする（`chloe ws root <名前> persistent`）。
- **git**：GitHub には HTTPS でつなぐ。`git@github.com:` の SSH のリモートも、HTTPS に読み替える。認証は、`GH_TOKEN` があれば自動で行う（「GitHub を使う」を参照）。

## VM が止まるとき

最後の接続が切れてから一定の時間（既定は 30 分）が経つと、VM を止める。
Claude Code や Codex が作業中のあいだは止めず、作業が終わってから止める（既定では最長 12 時間待つ）。
ポート転送をしている SSH の接続があるあいだも止めない。

VM を止めると、VM の中で動かしていたプロセス（開発用のサーバー、tmux のセッション）は終わる。
ファイルは「VM の中」で書いたとおりに残る。

すぐに止めたいときや、ルートの扱いを変えたあとで起動し直したいときは、`chloe ws stop <名前>` で止める。

## シークレット

API キーと GitHub のトークンは、VM に入れない。
VM の中の環境変数には、本物の値の代わりにプレースホルダーが入る。
VM からの通信を中継するプロキシが、決まった宛先の決まったヘッダーにあるプレースホルダーだけを、本物の値に差し替える。
VM の中のプログラムやエージェントが本物の値を読み出したり、ほかの宛先に送ったりはできない。

| 種類 | 環境変数 | 差し替える宛先 |
| --- | --- | --- |
| `anthropic_api_key` | `ANTHROPIC_API_KEY` | `api.anthropic.com` |
| `claude_oauth_token` | `CLAUDE_CODE_OAUTH_TOKEN` | `api.anthropic.com` |
| `openai_api_key` | `OPENAI_API_KEY`、`CODEX_API_KEY` | `api.openai.com` |
| `github_token` | `GH_TOKEN`、`GITHUB_TOKEN` | `github.com`、`api.github.com`、`uploads.github.com` |

値は標準入力で渡す。
SSH の接続は VM を通らないので、値は VM に届かない。

```sh
ssh alice@chloe.example.com chloe secret set anthropic_api_key < key.txt
ssh alice@chloe.example.com chloe secret list
ssh alice@chloe.example.com chloe secret rm anthropic_api_key
```

登録した値は、次の要求から使われる。
動いているワークスペースでは、このあとに開いたシェルに新しいプレースホルダーが入る（すでに開いているシェルの環境は変わらない）。

### 認証情報の置き場所

| 認証情報 | 置き場所 |
| --- | --- |
| API キー、`claude_oauth_token` | コントロールプレーン（暗号化して保存）。VM にはプレースホルダーだけ |
| GitHub のトークン | コントロールプレーン。VM にはプレースホルダーだけ |
| Codex の ChatGPT のログインなど、VM の中でログインしたもの | VM のホームディレクトリ |

Claude Code をサブスクリプションで使うには、手元で `claude setup-token` を実行して得たトークンを `claude_oauth_token` に登録する。
Codex を ChatGPT のアカウントで使うには、VM の中で `codex login` を実行する（認証情報は VM のホームディレクトリに残る）。

### 共有ワークスペースのシークレット

Organization や team のワークスペースでは、その Organization や team のシークレットを使う。
個人のシークレットは、共有ワークスペースには入らない。
team のワークスペースは、team に登録がなければ Organization のシークレットを使う。

`--owner` で持ち主を指定する（`acme` か `acme/dev`）。
Organization のシークレットは Organization の管理者が、team のシークレットは Organization の管理者と team の maintainer が登録できる。
メンバーは名前の一覧を見られるが、値は読めない。

```sh
ssh alice@chloe.example.com chloe secret set --owner acme/dev anthropic_api_key < key.txt
```

## GitHub を使う

サーバーに GitHub App を設定していれば、次のように使える。

- **個人のワークスペース**：一度 `ssh alice@chloe.example.com chloe github login` を実行し、表示されたコードを GitHub で承認する。以後、`git` と、`GH_TOKEN` を読むツール（GitHub の API を呼ぶスクリプトや、自分で入れた `gh` など）が、自分の権限で GitHub を使える。トークンはコントロールプレーンが更新する。
- **共有ワークスペース**：GitHub App のインストールトークンを使う（操作はアプリの名前で記録される）。リポジトリと権限は、Organization の管理者が絞れる。

GitHub App がなければ、個人のアクセストークンを `github_token` に登録して使う。

## 外への通信

VM から外へは、許可リストにある宛先にだけつなげる。
許可リストにない名前は、名前の解決からできない（`Could not resolve host` になる）。
宛先を足したいときは、サーバーの運用者か、Organization の管理者に頼む（Organization と team の設定は `chloe policy` で変える）。

HTTPS の通信はプロキシで復号するので、VM の中には chloe の CA の証明書が入っている。
通信の内容（本文）は記録しない。
宛先、メソッド、パス、ステータスなどは、ワークスペースごとに記録する。

## ポート転送

VM の中で動かしているサーバーに、手元からつなげる。

```sh
ssh -N -L 8080:127.0.0.1:3000 alice@chloe.example.com
# 手元の http://localhost:8080 が、VM の中の 127.0.0.1:3000 につながる。
```

`-L` の接続先は、VM の中から見た宛先である。
手元から VM の外へ抜ける経路にはならない（VM の中からつなげる宛先にしかつながらない）。
リモートの転送（`-R`）はまだできない。

## `chloe` コマンド

`chloe` で始まるコマンドは、VM ではなくゲートウェイが受け取る。

```text
chloe ws list                                   ワークスペースの一覧
chloe ws create NAME [--root persistent|ephemeral]
chloe ws root NAME persistent|ephemeral          ルートの扱い (次の起動から)
chloe ws stop NAME
chloe ws rm NAME
chloe secret list [--owner OWNER]
chloe secret set [--owner OWNER] KIND            値は標準入力から読む
chloe secret rm [--owner OWNER] KIND
chloe policy show OWNER                          Organization か team の設定
chloe policy set OWNER                           TOML を標準入力から読む
chloe policy rm OWNER
chloe token create NAME [--expires 30d]          管理 API のトークン
chloe token list
chloe token revoke ID
chloe github login|status|logout
```

NAME は SSH のユーザー名の `+` のあとと同じ書き方、OWNER は Organization（`acme`）か team（`acme/dev`）。
`--root` を省くと、Organization と team の設定の既定を使う（設定がなければ `ephemeral`）。
同じ操作を HTTP(S) で行う方法は [管理 API](api.md) にある。

## 共有ワークスペースで気をつけること

共有ワークスペースの VM の中では、メンバーの全員が `sudo` と Docker を使える。
メンバーは互いに、VM の中では root と同じことができる。
共有ワークスペースは、そこに入れる全員を信頼できるときに使う。
誰がいつ接続したかは、ゲートウェイの監査ログに残る。
