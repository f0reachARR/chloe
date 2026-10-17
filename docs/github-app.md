# GitHub App の準備

chloe は一つの GitHub App を、次の三つの用途に使う。

- **Device Flow**：SSH の `keyboard-interactive` で利用者を認証し、個人ワークスペースの GitHub の認証情報（ユーザートークン）を得る。
- **所属の確認**：利用者が Organization や team に属しているかを、インストールトークンで確かめる。
- **共有ワークスペースの GitHub の認証情報**：ワークスペースごとにリポジトリと権限を絞ったインストールトークンを発行する。

## 登録する場所

アプリは Organization の下に登録する（Organization の Settings → Developer settings → GitHub Apps → New GitHub App）。
個人アカウントの下に登録すると、Organization へのインストールに Organization 側の承認が要る。

## 登録フォームの設定

| 項目 | 設定 |
| --- | --- |
| GitHub App name、Homepage URL | 任意 |
| Callback URL | 空のまま（Device Flow だけを使うので要らない） |
| Expire user authorization tokens | 有効（ユーザートークンを 8 時間で失効させ、リフレッシュトークンで更新する） |
| Request user authorization (OAuth) during installation | 無効 |
| Enable Device Flow | 有効 |
| Webhook の Active | 無効（chloe は Webhook を受けない） |
| Where can this GitHub App be installed? | Only on this account（複数の Organization で使うときは下の「複数の Organization」を参照） |

## 権限

最初は次の権限で始め、足りないものはあとから足す。
アプリの権限を変えると、インストール先の Organization で承認し直す必要がある。

| 種類 | 権限 | 設定 | 用途 |
| --- | --- | --- | --- |
| Repository | Contents | Read and write | clone、fetch、push |
| Repository | Metadata | Read-only | 必須（自動で付く） |
| Repository | Pull requests | Read and write | pull request の作成とレビュー |
| Repository | Issues | Read and write | issue の参照とコメント |
| Repository | Actions | Read-only | CI の結果とログの参照 |
| Repository | Checks | Read-only | CI の結果の参照 |
| Repository | Commit statuses | Read-only | CI の結果の参照 |
| Repository | Workflows | No access | `.github/workflows` の変更を許さない |
| Organization | Members | Read-only | Organization と team の所属の確認 |

Workflows を No access にすると、`.github/workflows` 以下を変更するコミットの push を GitHub が拒否する。
エージェントに CI の定義を書き換えさせない既定として、この設定から始める。

ユーザートークンの権限は、アプリの権限と利用者自身の権限の重なりになる。
共有ワークスペースのインストールトークンは、ここで与えた権限の範囲内で、Organization の管理者がワークスペースごとにさらに絞る。

## 登録後に控える情報

| 情報 | 取得する場所 | 秘密か |
| --- | --- | --- |
| App ID | アプリの設定画面の About | 秘密ではない |
| Client ID | アプリの設定画面の About | 秘密ではない |
| 秘密鍵（`.pem`） | アプリの設定画面の Private keys → Generate a private key | **秘密** |
| インストール先の Organization 名 | アプリをインストールした Organization | 秘密ではない |

Client secret は要らない。
Device Flow で得たユーザートークンは、client secret なしで更新できる。

アプリは Organization にインストールし、chloe で扱うリポジトリを選ぶ（Only select repositories）。
インストール ID は chloe がアプリの秘密鍵で API から調べるので、控えなくてよい。

## 情報の置き場

秘密鍵はチャットやリポジトリに貼らず、ゲートウェイを動かす利用者だけが読めるようにする。

### ホストに配置するとき

[インストール](install.md) の配置では、`/etc/chloe/` に置き、ゲートウェイを動かす利用者 `chloe` のグループに読ませる。

```sh
sudo install -m 640 -g chloe ~/Downloads/<app-name>.*.private-key.pem /etc/chloe/github-app.pem
```

`/etc/chloe/github-app.toml`：

```toml
app_id = 123456
client_id = "Iv23liXXXXXXXXXXXXXX"
private_key = "file:/etc/chloe/github-app.pem"
# 使う Organization を限るときに書く。書かなければ、アプリをインストールしたすべての
# Organization を使う。
# organizations = ["ForteFibre"]
```

`private_key` はシークレットの参照として読む。
1Password Connect を使うようになったら、`op://<vault>/<item>/<field>` の形の参照に書き換え、秘密鍵のファイルは消す。

ゲートウェイの設定（`/etc/chloe/gateway.toml`）にこのパスを書く。

```toml
github_app = "/etc/chloe/github-app.toml"
```

### 開発するとき

リポジトリの外の `~/.config/chloe/` に置く。

```sh
mkdir -p ~/.config/chloe
chmod 700 ~/.config/chloe
mv ~/Downloads/<app-name>.*.private-key.pem ~/.config/chloe/github-app.pem
chmod 600 ~/.config/chloe/github-app.pem
```

`~/.config/chloe/github-app.toml` の書き方は上と同じで、`private_key` を `file:/home/<user>/.config/chloe/github-app.pem` にする。
ゲートウェイの設定には `github_app = "~/.config/chloe/github-app.toml"` と書く（`~` はゲートウェイを起動した利用者のホームディレクトリに読み替える）。

## ゲートウェイでの使い方

ゲートウェイの設定の `github_app` に GitHub App の設定のパスを書くと、次のようになる。

- アプリをインストールしたいずれかの Organization のメンバーがログインできる。`allowed_users` は、それに加えて許す利用者になる。
- Organization のワークスペース（`org/name`）にはそのメンバーが、team のワークスペース（`org/team/name`）にはその team のメンバーが入れる。確かめられるのはアプリをインストールした Organization だけである。
- SSH でログインするには、一度 keyboard-interactive で Device Flow を済ませる。GitHub に登録した公開鍵でも、ユーザートークンがなければ Device Flow に進める。
- `ssh_user`（既定は `chloe`）と GitHub に存在しない名前のユーザー名では、Device Flow で承認したアカウントを利用者にし、そのとき使った公開鍵を結び付ける。次からはその鍵で入れる。
- 個人のワークスペースには、Device Flow で得たユーザートークンを使う。トークンを取り直すときは `ssh <host> chloe github login` を実行する。トークンは chloe のデータベースに暗号化して置き、ランナーのプロキシが差し替えのたびに問い合わせたとき、失効まで 5 分を切っていれば更新してから答える。VM にはプレースホルダーだけを入れる。
- team と Organization のワークスペースには、その Organization へのインストールのトークンを渡す。範囲は Organization と team の設定（`chloe policy set`）の `[github]` で絞れる。
- GitHub で Organization の管理者（`admin` の役割）である利用者は、その Organization と team の設定とシークレットを変えられる。team の maintainer は、その team のシークレットを変えられる。

VM の中の `git` は、GitHub に HTTPS でつなぎ、認証ヘルパーがプレースホルダーを返す（SSH のリモートも HTTPS に読み替える）。

設定は `cargo run -p chloe-github --example check -- ~/.config/chloe/github-app.toml <login>` で確かめられる（トークンの値は表示しない）。

## 複数の Organization

chloe は、アプリをインストールしたすべての Organization を使う（App の設定の `organizations` で限っていなければ）。
アプリを Only on this account で登録すると、インストールできるのは登録した Organization だけになる。
ほかの Organization でも使うには、アプリを Any account にして、その Organization にインストールする。

Any account にすると、だれでも自分の Organization にアプリをインストールでき、そのメンバーが chloe にログインして VM を使えるようになる。
このときは App の設定の `organizations` に、使ってよい Organization を並べて限る。

