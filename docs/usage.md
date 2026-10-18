# 使い方

chloe に SSH で入って使う人のための説明である。
サーバーを用意する手順は [インストール](install.md) にある。

## 接続する

ユーザー名を `chloe` にして入る（`git@github.com` と同じく、誰でも同じユーザー名を使う）。
初めて入るときは、SSH が GitHub へのサインインを求める。
Enter を押すと Device Flow のコードが表示される。
`https://github.com/login/device` でコードを入力して承認し、もう一度 Enter を押す。
Device Flow は一つの接続で一度だけ行う。失敗したり、ほかのアカウントで承認したりしたときは、接続し直す。
承認した GitHub のアカウントが、chloe での利用者になる。

```sh
ssh chloe@chloe.example.com
```

承認したときに使っていた公開鍵は、その利用者に結び付く。
次からは、同じ鍵で Device Flow なしに入れる。
鍵を持っていなければ、毎回 Device Flow になる。
ssh-agent に鍵がいくつもあるときは、クライアントが最初に出した鍵が結び付く（`-i` と `IdentitiesOnly yes` で鍵を決めておくとよい）。

GitHub のログイン名をユーザー名にしても入れる（`ssh alice@chloe.example.com`）。
GitHub に登録した公開鍵か、結び付けた鍵で入れるが、一度は Device Flow を済ませる必要がある。
鍵を結び付けるのは `chloe` で入ったときだけである。GitHub に登録していない鍵で `alice` として入ると、毎回 Device Flow になる。
GitHub のトークンが失効したり、`chloe github logout` で消したりすると、次の接続でまた Device Flow になる。
GitHub に存在しない名前は、`chloe` と同じに扱う。

ユーザー名の `+` のあとに、入るワークスペースを書ける。
書かなければ、自分の個人ワークスペース `default` に入る（なければ作る）。
ただし、端末を割り当ててシェルを開いたとき（`ssh` にコマンドを渡さないとき）に、`default` のほかにも入れるワークスペースがあれば、入るワークスペースを選ぶ画面が出る。
↑↓ で選んで Enter で入り、q か Esc で閉じる。
m で管理の TUI（`ssh -t chloe@chloe.example.com chloe` で開くものと同じ）を開き、q で閉じると選ぶ画面に戻る。
管理の TUI で作ったワークスペースは、戻った選ぶ画面に並ぶ。
選んだワークスペースは、同じ接続のあとのシェルとポート転送でも使う。
コマンドを渡したときと、シェルを開かないポート転送だけの接続（`ssh -N -L`）では、選ぶ画面は出ずに `default` に入る。

| ユーザー名 | 入るワークスペース |
| --- | --- |
| `chloe` か `alice` | 自分の個人ワークスペース `default`（端末では選ぶ画面が出ることがある） |
| `chloe+api` | 自分の個人ワークスペース `api` |
| `chloe+acme/web` | Organization acme の共有ワークスペース `web` |
| `chloe+acme/dev/tools` | acme の team dev の共有ワークスペース `tools` |

`default` 以外のワークスペースは、先に `chloe ws create` で作る。
コマンドを渡すと、そのワークスペースで実行して終わる（`ssh chloe@chloe.example.com 'git status'`）。
誰として入ったかは `chloe whoami` で、結び付いた鍵は `chloe key list` で分かる。
鍵をなくしたときは、`chloe key rm <指紋>` で結び付けを消す。

サーバーに GitHub App を設定していなければ、Device Flow は使えない。
GitHub のログイン名をユーザー名にして、GitHub に登録した公開鍵で入る。

止まっていたワークスペースは、接続したときに起動する（数秒かかる）。

## VM の中

ワークスペースごとに一つの VM がある。

- **OS と利用者**：Ubuntu 26.04。利用者は `user` で、`sudo` を使える。
- **ホスト名**：ワークスペースの名前になり、プロンプトに出る（`acme/dev/tools` は `acme-dev-tools`）。正確な名前は環境変数 `CHLOE_WORKSPACE` にある。
- **入っているもの**：Docker、[mise](https://mise.jdx.dev/)、git、Python 3、curl、tmux など。Claude Code や Codex などのツールは入っていないので、mise で入れる（「ツールを入れる」を参照）。
- **残るもの**：`/workspace` と、ホームディレクトリ（`/home/user`）は、ワークスペースのディスクにあり、VM を止めても残る。Docker のイメージとコンテナのデータも、別のディスクに残る。
- **ディスクの大きさ**：既定ではワークスペースのディスクが 20GiB、Docker のディスクが 40GiB（ゲートウェイの設定で変わる）。足りなくなったら `chloe ws disk <名前> --workspace 50G` のように大きくする（「ディスクを大きくする」を参照）。
- **残らないもの**：ルートファイルシステムへの変更（`apt install` で入れたものなど）は、既定では VM を止めると消える。残したいときは、ワークスペースのルートの扱いを `persistent` にする（`chloe ws root <名前> persistent`）。
- **git**：GitHub には HTTPS でつなぐ。`git@github.com:` の SSH のリモートも、HTTPS に読み替える。認証は、`GH_TOKEN` があれば自動で行う（「GitHub を使う」を参照）。

## ツールを入れる

コーディングエージェントや言語の処理系は、VM の中で mise で入れる。
入れたものはホームディレクトリ（`~/.local/share/mise`）に置かれ、ワークスペースのディスクにあるので、VM を止めても残る。

```sh
mise use -g jq            # すべてのディレクトリで使う (~/.config/mise/config.toml に書く)
mise use node@24          # このディレクトリで使う (mise.toml に書く)
```

端末を開いたときも、`ssh <ホスト> 'コマンド'` で実行したときも、mise で入れたツールを使える。
ほかの人が書いた `mise.toml`（リポジトリに入っているものなど）は、`mise trust` で信頼するまで読まない。

mise がツールを取りに行く宛先も、外への通信の規則に従う。
既定の規則が許しているのは、GitHub（`github.com`、`*.githubusercontent.com`）、mise の版の一覧（`mise-versions.jdx.dev`）、npm と PyPI と crates.io である。
ほかの宛先（Node.js の `nodejs.org` など）から取るツールは、規則に宛先を足してから入れる（「外への通信」を参照）。
GitHub の API は、`GITHUB_TOKEN` があれば認証して呼ぶので、回数の制限にかかりにくい。

Claude Code と Codex は、作業中であることを VM に知らせるフックをあらかじめ設定してある（`/etc/claude-code/managed-settings.d/`、`/etc/codex/hooks.json`）。
mise で入れれば、そのまま作業中のあいだは VM を止めない（「VM が止まるとき」を参照）。

## VM が止まるとき

最後の接続が切れてから一定の時間（既定は 30 分。ゲートウェイの設定で変わる）が経つと、VM を止める。
Claude Code や Codex が作業中のあいだは止めず、作業が終わってから止める（既定では最長 12 時間待つ）。
ポート転送をしている SSH の接続があるあいだも止めない。

VM を止めると、VM の中で動かしていたプロセス（開発用のサーバー、tmux のセッション）は終わる。
ファイルは「VM の中」で書いたとおりに残る。

すぐに止めたいときや、ルートの扱い、ディスクや VM の大きさを変えたあとで起動し直したいときは、`chloe ws stop <名前>` で止める。

止めるまでの時間は、`chloe ws idle` でワークスペースごとに変えられる。
`30m`、`2h`、`1d` のように書き、`never` なら接続がなくても止めず、`default` ならゲートウェイの設定の時間に戻す。

```sh
ssh chloe@chloe.example.com chloe ws idle api 2h
ssh chloe@chloe.example.com chloe ws idle api never   # chloe ws stop で止めるまで動かし続ける
ssh chloe@chloe.example.com chloe ws idle api         # 今の時間を出す
```

VM が動いていて接続がなければ、変えたときから数え直す。

## VM の大きさを変える

`chloe ws machine` で、VM の vCPU の数とメモリを変える。
vCPU は 1 から 32、メモリは `8G` や `512M` のように書く（512M 以上）。
書かなかったほうは変えない。
既定は 4 vCPU とメモリ 4GiB である（ゲートウェイの設定で変わる）。

```sh
ssh chloe@chloe.example.com chloe ws machine api --cpus 8 --memory 16G
ssh chloe@chloe.example.com chloe ws machine api     # 今の大きさを出す
```

ディスクと同じく、新しい大きさは次に VM を起動したときに効く。
すぐに変えたいときは、`chloe ws stop <名前>` で止めてからつなぎ直す。
作るときに決めるなら、`chloe ws create <名前> --cpus 8 --memory 16G --idle 2h` のように書く。

## ディスクを大きくする

`chloe ws disk` で、ワークスペースのディスクと Docker のディスクを大きくする。
大きさは GiB で書く（`50G`、`50GiB`、`50` のどれでもよい）。
書かなかったほうのディスクは変えない。

```sh
ssh chloe@chloe.example.com chloe ws disk api --workspace 50G --docker 80G
ssh chloe@chloe.example.com chloe ws disk api     # 今の大きさを出す
```

新しい大きさは、次に VM を起動したときに効く。
VM が動いているあいだはディスクを広げられないためである（起動する前に、ファイルシステムを検査してから広げる）。
すぐに広げたいときは、`chloe ws stop <名前>` で止めてからつなぎ直す。

ディスクは大きくするだけで、小さくはできない。
ext4 を縮めるには時間がかかり、中身が収まらなければ失敗するためである。
作り直したいときは、`chloe ws rm <名前> --disks` でディスクごと消してから作る。
大きさの上限は、ゲートウェイの設定（`[disks]`）で決まる。
作るときに大きさを決めるなら、`chloe ws create <名前> --workspace 50G` のように書く。

永続のルート（`persistent`）で残す変更も、ワークスペースのディスクに入る。
`apt install` で多くのものを入れるときは、ワークスペースのディスクを大きめにしておく。

## シークレット

API キーと GitHub のトークンは、VM に入れない。
VM の中の環境変数には、本物の値の代わりにプレースホルダーが入る。
VM からの通信を中継するプロキシが、決まった宛先の決まったヘッダーにあるプレースホルダーだけを、本物の値に差し替える。
VM の中のプログラムやエージェントが本物の値を読み出したり、ほかの宛先に送ったりはできない。

シークレットは、定義と値を TOML で標準入力から渡して登録する。
定義には、プレースホルダーを入れる VM の環境変数、プレースホルダーの形、差し替える宛先と場所を書く。
SSH の接続は VM を通らないので、値は VM に届かない。

```toml
# hf.toml
env = ["HF_TOKEN"]             # プレースホルダーを入れる VM の環境変数
placeholder = "hf_{id}"        # プレースホルダーの形。{id} をワークスペースごとの乱数に置き換える
value = "hf_..."               # 本物の値

[[inject]]
hosts = ["huggingface.co", "*.huggingface.co"]
header = "authorization"       # このヘッダーの中のプレースホルダーを本物に置き換える
# format = "basic-password"    # Basic 認証のパスワードとして置き換えるとき
```

```sh
ssh chloe@chloe.example.com chloe secret set hf_token --spec < hf.toml
ssh chloe@chloe.example.com chloe secret set hf_token < new-value.txt   # 値だけを変える（定義はそのまま）
ssh chloe@chloe.example.com chloe secret list
ssh chloe@chloe.example.com chloe secret show hf_token   # 定義を見る（値は出ない）
ssh chloe@chloe.example.com chloe secret rm hf_token
```

`deploy/secrets/` に、よく使うものの定義の例がある。
`value` の行を本物の値に書き換えて登録する。

| 例 | 環境変数 | 差し替える宛先 |
| --- | --- | --- |
| `anthropic_api_key.toml` | `ANTHROPIC_API_KEY` | `api.anthropic.com` |
| `claude_oauth_token.toml` | `CLAUDE_CODE_OAUTH_TOKEN` | `api.anthropic.com` |
| `openai_api_key.toml` | `OPENAI_API_KEY`、`CODEX_API_KEY` | `api.openai.com` |
| `aws.toml` | `AWS_ACCESS_KEY_ID`、`AWS_SECRET_ACCESS_KEY` | `*.amazonaws.com`（[署名](#署名する方式aws)） |
| `apt.toml` | なし（[ファイル](#環境変数を読まないツール)の `/etc/apt/auth.conf.d/`） | 認証のある apt のリポジトリ |

```sh
ssh chloe@chloe.example.com chloe secret set anthropic_api_key --spec < anthropic_api_key.toml
```

登録した値は、次の要求から使われる。
動いているワークスペースでは、このあとに開いたシェルに新しいプレースホルダーが入る（すでに開いているシェルの環境は変わらない）。

- **名前**：英小文字で始め、英小文字、数字、`_` で書く（`_` は続けず、末尾にも置かない）。
- **placeholder**：`{id}` を一つだけ含める。ほかに使える文字は英数字と `-_.~+/=:`。省くと `{id}` だけになる。ツールが手元で形式を調べることがあるので、本物の値の接頭辞に合わせるとよい。
- **inject**：`hosts` への要求で、`header` の中のプレースホルダーを本物に置き換える。`hosts` は `*.example.com` の形でサブドメインを指定できる。`format = "basic-password"` では、Basic 認証のユーザー名も欄にして差し替えられる（[ユーザー名も差し替える](#ユーザー名も差し替える)）。
- 規則の宛先は、そのシークレットを使うワークスペースで自動で許す。ただし、ゲートウェイや Organization の規則にすでに書いた宛先と重なる宛先は、その規則で決まる。
- 規則の宛先以外へプレースホルダーを送ると、プロキシが要求を遮断する。ヘッダーと URL のほか、本文も 1 MiB までなら調べる。

定義を変えるときは、`--spec` で定義と値をまとめて渡し直す。
GitHub のトークン（`github_token`）は、定義を付けずに値だけを登録する（[GitHub を使う](#github-を使う)）。
渡す環境変数や宛先は、運用者がゲートウェイの設定の [`[github_token]`](configuration.md#github_token) で変える。

### 署名する方式（AWS）

AWS のように、鍵で要求に署名する方式では、プレースホルダーをヘッダーに置き換えるだけでは足りない。
VM の中のツールはプレースホルダーの鍵で署名し、プロキシが本物の鍵で署名を作り直す。
値が複数あるので、欄（`fields`）ごとに環境変数、プレースホルダー、値を書く（例は `deploy/secrets/aws.toml`）。

```toml
# aws.toml
[fields.access_key_id]
env = ["AWS_ACCESS_KEY_ID"]
placeholder = "AKIA{id}"
value = "AKIA..."

[fields.secret_access_key]
env = ["AWS_SECRET_ACCESS_KEY"]
value = "..."

# 一時的な認証情報なら、セッショントークンの欄も足す。
# [fields.session_token]
# env = ["AWS_SESSION_TOKEN"]
# value = "..."

[[sign]]
scheme = "aws-sigv4"
hosts = ["*.amazonaws.com"]
```

```sh
ssh chloe@chloe.example.com chloe secret set aws --spec < aws.toml
```

値だけを変えるときも、欄ごとに `value` だけを書いた TOML を `--spec` で渡す。

`aws-sigv4` は、欄 `access_key_id` と `secret_access_key` を使い、欄 `session_token` があれば `x-amz-security-token` ヘッダーも置き換える。
次の要求は署名を作り直せないので、プロキシが遮断する（ログの理由は `unsignable`）。

- 署名付き URL（クエリの `X-Amz-Credential` で署名するもの）
- チャンクごとに署名する要求（`x-amz-content-sha256: STREAMING-AWS4-HMAC-SHA256-PAYLOAD` など）
- 本文が 1 MiB を超えるか大きさが分からず、`x-amz-content-sha256` ヘッダーもない要求（本文のハッシュを計算できない）

S3 の SDK は `x-amz-content-sha256` を付けるので、大きなアップロードでも通る。
チャンクに署名しない `STREAMING-UNSIGNED-PAYLOAD-TRAILER`（新しい SDK がチェックサムを付けてアップロードするときの方式）も通る。

### 環境変数を読まないツール

apt のように、認証情報を設定ファイルから読むツールには、プレースホルダーを入れたファイル（`files`）を VM に置く。
ツールはファイルのプレースホルダーを送り、プロキシが環境変数のときと同じ規則で本物に差し替える。
次の例では、apt が `auth.conf.d` のパスワードを Basic 認証で送るので、`basic-password` で差し替える（例は `deploy/secrets/apt.toml`）。

```toml
# apt.toml
value = "..."

[[inject]]
hosts = ["apt.example.com"]
header = "authorization"
format = "basic-password"

[[files]]
path = "/etc/apt/auth.conf.d/chloe-apt.conf"
content = "machine apt.example.com login deploy password {value}\n"
```

- **path**：VM の中の絶対パス。`/run/chloe`、`/proc`、`/sys`、`/dev` の下には置けない。
- **content**：`{value}` を、そのシークレットのプレースホルダーに置き換える。欄がいくつもあるシークレットでは、`{access_key_id}` のように欄の名前で書く。欄の名前の形をしていない `{...}` は、そのまま残る。
- **owner**：`root`（既定）か `user`。ホームディレクトリのファイル（`/home/user/.netrc` など）は `user` にする。
- **mode**：8 進数で書く（`"0600"` など）。既定は `0644` である。プレースホルダーは登録した宛先にしか送れないので、誰が読めてもよい。

動いているワークスペースでは、登録するとすぐにファイルを置き直す。
シークレットを消したり、そのワークスペースで使えなくなったりすると、置いたファイルも消える。
ただし、ルートを起動のたびに捨てるワークスペース（`ephemeral`）では、VM を止めると置いたファイルの一覧も消える。
そのため、ワークスペースのディスク（`/home/user`、`/workspace`）に置いたファイルは、止めているあいだにシークレットを消すと残る。

apt のリポジトリは、`sources.list` で `https://` で書く。
`http://` だと、プロキシから先へ本物のパスワードが平文で流れる（`http://` の宛先で差し替えるときは、どのシークレットでも同じである）。

### ユーザー名も差し替える

`basic-password` は、ふつうはパスワードだけを差し替え、ユーザー名は VM の中のツールが送ったまま残す。
ユーザー名も VM に見せたくないときや、パスワードと一緒に変えたいときは、ユーザー名の欄を作り、規則の `username_field` にその欄の名前を書く。
`field` にはパスワードの欄を書く。

```toml
[fields.username]
env = ["REPO_USER"]
placeholder = "user-{id}"
value = "deploy"

[fields.password]
env = ["REPO_PASSWORD"]
value = "..."

[[inject]]
field = "password"
username_field = "username"
hosts = ["repo.example.com"]
header = "authorization"
format = "basic-password"
```

VM の中では、ユーザー名にもプレースホルダーが入る（`curl --user "$REPO_USER:$REPO_PASSWORD"` や、ファイルの `{username}`）。
プロキシは、Basic 認証のユーザー名とパスワードのうち、プレースホルダーになっているほうを本物に置き換える。
ユーザー名のプレースホルダーも、規則の宛先以外へ送ると遮断する。
Basic 認証はユーザー名とパスワードを `:` で分けるので、ユーザー名の欄の `placeholder` には `:` を書けない。

### 認証情報の置き場所

| 認証情報 | 置き場所 |
| --- | --- |
| API キーなど、登録したシークレット | コントロールプレーン（暗号化して保存）。VM にはプレースホルダーだけ |
| GitHub のトークン | コントロールプレーン。VM にはプレースホルダーだけ |
| Codex の ChatGPT のログインなど、VM の中でログインしたもの | VM のホームディレクトリ |

Claude Code をサブスクリプションで使うには、手元で `claude setup-token` を実行して得たトークンを、`deploy/secrets/claude_oauth_token.toml` の定義で登録する。
Codex を ChatGPT のアカウントで使うには、VM の中で `codex login` を実行する（認証情報は VM のホームディレクトリに残る）。

### 共有ワークスペースのシークレット

Organization や team のワークスペースでは、その Organization や team のシークレットを使う。
個人のシークレットは、共有ワークスペースには入らない。
team のワークスペースは、team に登録がなければ Organization のシークレットを使う。

`--owner` で持ち主を指定する（`acme` か `acme/dev`）。
Organization のシークレットは Organization の管理者が、team のシークレットは Organization の管理者と team の maintainer が登録できる。
メンバーは名前の一覧と定義を見られるが、値は読めない。
定義を付けたシークレットの値だけを変えるときは、同じ持ち主に置いた定義を使う（team に値だけを登録しても、Organization の定義は引き継がない）。

```sh
ssh chloe@chloe.example.com chloe secret set --owner acme/dev anthropic_api_key --spec < anthropic_api_key.toml
```

## GitHub を使う

サーバーに GitHub App を設定していれば、次のように使える。

- **個人のワークスペース**：ログインのときに Device Flow で得たトークンを使う。以後、`git` と、`GH_TOKEN` を読むツール（GitHub の API を呼ぶスクリプトや、自分で入れた `gh` など）が、自分の権限で GitHub を使える。トークンはコントロールプレーンが更新する。取り直すときは `ssh chloe@chloe.example.com chloe github login` を実行する。
- **共有ワークスペース**：GitHub App のインストールトークンを使う（操作はアプリの名前で記録される）。リポジトリと権限は、Organization の管理者が絞れる。

GitHub App がなければ、個人のアクセストークンを `github_token` に登録して使う。

## 外への通信

VM から外へは、規則で許した宛先にだけつなげる。
規則で許していない名前は、名前の解決からできない（`Could not resolve host` になる）。
宛先を足したいときは、サーバーの運用者か、Organization の管理者に頼む（Organization と team の設定は `chloe policy` で、ワークスペースの規則は `chloe ws policy` で変える）。
team の maintainer は、その team のワークスペースの規則を変えられる。
サーバーで有効にしていれば、自分の個人のワークスペースの宛先は自分で足せる（`chloe policy set` で個人の設定を、`chloe ws policy set NAME` でワークスペースの規則を変える）。
足せるのは、上の層（ゲートウェイや Organization）の規則にない宛先だけである（[設定の重ね方](configuration.md#設定の重ね方)）。

HTTPS の通信はプロキシで復号するので、VM の中には chloe の CA の証明書が入っている。
通信の内容（本文）は記録しない。
宛先、メソッド、パス、ステータスなどは、ワークスペースごとに記録する。

## ポート転送

VM の中で動かしているサーバーに、手元からつなげる。

```sh
ssh -N -L 8080:127.0.0.1:3000 chloe@chloe.example.com
# 手元の http://localhost:8080 が、VM の中の 127.0.0.1:3000 につながる。
```

`-L` の接続先は、VM の中から見た宛先である。
手元から VM の外へ抜ける経路にはならない（VM の中からつなげる宛先にしかつながらない）。
リモートの転送（`-R`）はまだできない。

## 管理の画面

端末を割り当てて `chloe` だけを実行すると、管理の画面（TUI）が開く。

```sh
ssh -t chloe@chloe.example.com chloe
```

ワークスペース、シークレット、規則、個人と Organization と team の設定、管理 API のトークン、結び付けた SSH の鍵、GitHub のサインインを、タブごとに扱える。
シークレットと設定は、TOML を書かずに欄ごとのフォームで編集する。
シークレットは、`deploy/secrets/` の例を雛形にして作れる。
登録したシークレットの定義を変えるとき、値の欄を空のままにすると今の値を使い続ける。
ワークスペースのタブでは、`g` でディスクの大きさを変える（`chloe ws disk` と同じく、次の起動から効く）。
`m` で、VM の vCPU の数とメモリ（次の起動から効く）と、止めるまでの時間を変える。
`u` で、選んだワークスペースを Rules タブで開く。

Rules タブでは、左の一覧でスコープ（ゲートウェイ、個人、Organization、team、ワークスペース）を選ぶ。
右には、そのスコープで効く規則を評価する順に、層の名前と一緒に出す（`▶` がそのスコープ自身の規則）。
`Enter` でそのスコープ自身の規則を編集し、`d` で消す。
一覧の `view` のスコープは、見るだけで変えられない。
Settings タブでは、規則のほか（ルートの扱いの既定とトークンの範囲）を編集する。

| キー | 操作 |
| --- | --- |
| `←` `→`、`1`〜`7` | タブを切り替える |
| `↑` `↓` | 一覧で選ぶ |
| `n`、`Enter`、`d` | 作る、編集する、消す（タブの下に、そのタブで使えるキーが出る） |
| `Tab`、`↑` `↓` | フォームの欄を移る |
| `←` `→` | フォームの選択の欄を切り替える |
| `Enter` | フォームの規則の一覧を開く（一覧では `a` で足し、`d` で消し、`Esc` で戻る） |
| `Ctrl+S`、`Esc` | フォームを保存する、閉じる |
| `q`、`Ctrl+C` | 終わる |

`~/.ssh/config` に次のように書いておくと、`ssh chloe-admin` で開ける。

```text
Host chloe-admin
    HostName chloe.example.com
    User chloe
    RequestTTY yes
    RemoteCommand chloe
```

端末を割り当てないとき（`ssh chloe@chloe.example.com chloe`）は、コマンドの一覧を出す。

## `chloe` コマンド

`chloe` で始まるコマンドは、VM ではなくゲートウェイが受け取る。

```text
chloe whoami                                    接続した利用者の GitHub のログイン名
chloe ws list                                   ワークスペースの一覧
chloe ws create NAME [--root persistent|ephemeral] [--workspace SIZE] [--docker SIZE]
                     [--cpus N] [--memory MEM] [--idle TIME]
chloe ws root NAME persistent|ephemeral          ルートの扱い (次の起動から)
chloe ws disk NAME [--workspace SIZE] [--docker SIZE]   ディスクを大きくする (次の起動から)
chloe ws machine NAME [--cpus N] [--memory MEM]  VM の大きさを変える (次の起動から)
chloe ws idle NAME [TIME]                        最後の接続が切れてから止めるまで
chloe ws policy show NAME                        ワークスペースの規則
chloe ws policy set NAME                         TOML を標準入力から読む
chloe ws policy rm NAME
chloe ws stop NAME
chloe ws rm NAME [--disks]                      --disks ならディスクも消す
chloe secret list [--owner OWNER]
chloe secret set [--owner OWNER] KIND            値は標準入力から読む
chloe secret rm [--owner OWNER] KIND
chloe policy show [OWNER]                        個人、Organization か team の設定
chloe policy set [OWNER]                         TOML を標準入力から読む
chloe policy rm [OWNER]
chloe token create NAME [--expires 30d]          管理 API のトークン
chloe token list
chloe token revoke ID
chloe key list                                  Device Flow で結び付けた SSH の公開鍵
chloe key rm FINGERPRINT
chloe github login|status|logout
```

NAME は SSH のユーザー名の `+` のあとと同じ書き方、OWNER は Organization（`acme`）か team（`acme/dev`）。
`secret` と `policy` は、OWNER を省くと自分の個人のものを扱う。
`--root` を省くと、持ち主（個人、Organization と team）の設定の既定を使う（設定がなければ `ephemeral`）。
SIZE は GiB で書く（`50G`）。省いたディスクは、ゲートウェイの設定の既定の大きさで作る。
MEM は `8G` か `512M` のように、TIME は `30m`、`2h`、`1d`、`never`、`default` のように書く。省くとゲートウェイの設定の既定を使う。
`chloe ws list` は、名前、ルートの扱い、動いているか、ワークスペースと Docker のディスクの大きさ、vCPU の数、メモリ、止めるまでの時間を、タブで区切って出す。
`chloe ws rm` は、`--disks` を付けなければワークスペースのディスク（`/workspace`、ホームディレクトリ、Docker のデータ）をランナーのホストに残す。
管理の画面で消すときは、ディスクを残す（`k`）か一緒に消す（`d`）かを選ぶ。
同じ操作を HTTP(S) で行う方法は [管理 API](api.md) にある。

## 共有ワークスペースで気をつけること

共有ワークスペースの VM の中では、メンバーの全員が `sudo` と Docker を使える。
メンバーは互いに、VM の中では root と同じことができる。
共有ワークスペースは、そこに入れる全員を信頼できるときに使う。
誰がいつ接続したかは、ゲートウェイの監査ログに残る。
