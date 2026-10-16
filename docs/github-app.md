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
| Where can this GitHub App be installed? | Only on this account |

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

## 情報の置き場（開発用）

リポジトリの外の `~/.config/chloe/` に置く。
秘密鍵はチャットやリポジトリに貼らず、ファイルの権限を 600 にする。

```sh
mkdir -p ~/.config/chloe
chmod 700 ~/.config/chloe
mv ~/Downloads/<app-name>.*.private-key.pem ~/.config/chloe/github-app.pem
chmod 600 ~/.config/chloe/github-app.pem
```

`~/.config/chloe/github-app.toml`：

```toml
app_id = 123456
client_id = "Iv23liXXXXXXXXXXXXXX"
organization = "ForteFibre"
private_key = "file:/home/<user>/.config/chloe/github-app.pem"
```

`private_key` はシークレットの参照として読む。
1Password Connect を使うようになったら、`op://<vault>/<item>/<field>` の形の参照に書き換え、秘密鍵のファイルは消す。
