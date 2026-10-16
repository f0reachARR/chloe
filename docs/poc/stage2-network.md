# 段階 2：ネットワークの検証結果

[計画書](../research-and-plan.md) の PoC 確認項目のうち、本物の認証情報なしで確かめられるものを検証した。
検証日は 2026-10-16 である。

## 構成

段階 1 の VM を、ランナーの proxy モード（`CHLOE_NET_MODE=proxy`）で起動する。

- ランナーのコンテナの中に tap0（172.29.0.1/30）を作り、VM は 172.29.0.2 を使う。
- nftables で、tap0 から来る 80 番と 443 番の TCP を mitmproxy 12.2.3 の transparent モード（8080 番）へ転送する。DNS（53 番）はランナーの dnsmasq へ転送する。それ以外はすべて遮断し、転送（forward）もしない。
- chloe の方針は mitmproxy のアドオン（`images/guest/dev/proxy/chloe_policy.py`）として書き、規則は `policy.dev.json` に置く。
- 本物の API キーは使わない。プロキシは偽の秘密情報を埋め込み、差し替えの結果は httpbin.org の応答（受け取ったヘッダーを返す）で確かめる。

再現は `images/guest/dev/net-test.sh` で行う。

## 結果

| 確認項目 | 結果 |
| --- | --- |
| CA 証明書の公開（`http://ca.chloe.internal/ca.pem`）と信頼ストアへの取り込み | 成功 |
| 許可した宛先への HTTPS（200）、許可していない宛先の遮断（403） | 期待どおり |
| HTTP/2（`curl --http2`）と平文の HTTP | 期待どおり |
| 登録した宛先でのプレースホルダーの差し替え | 本物（偽の秘密情報）に置き換わった |
| 登録していない宛先へのプレースホルダー（ヘッダー、クエリ、本文） | すべて 403 で遮断 |
| 登録した宛先でも、規則にないヘッダーのプレースホルダー | 差し替えずにそのまま届く |
| SNI と Host の食い違い（ドメインフロンティング） | 403 で遮断 |
| ゲストが別の IP アドレスへ接続した場合 | SNI のホストに接続した（元の IP アドレスは使わない） |
| 80 番と 443 番以外への通信 | 届かない |
| 中継のみの宛先（Docker Hub） | 復号せず、本物の証明書のまま届いた |
| `docker pull` | 成功 |
| CA を入れていないコンテナから復号対象の宛先への HTTPS | 証明書の検証で失敗（想定どおり） |
| コンテナが `ca.chloe.internal` から CA を取得した場合 | 成功 |
| Claude Code（`ANTHROPIC_API_KEY` にプレースホルダー） | API に届き、差し替えた偽のキーで `Invalid API key` |
| Codex（`CODEX_API_KEY` にプレースホルダー） | API に届き、差し替えた偽のキーで `Incorrect API key provided: sk-proj-****-key` |
| bubblewrap と `codex sandbox` が VM の中で動くか | 動く（段階 1 の smoke-test.sh で確認） |

## 分かったこと

**Claude Code の通信先**：`api.anthropic.com` だけで動いた。
モデルの呼び出し（`/v1/messages`）のほか、設定、ポリシー、イベントログ、MCP レジストリなどの API も同じホストにある。
API キーの差し替えは、モデルの呼び出し以外の API にも適用される。
Datadog（`http-intake.logs.us5.datadoghq.com`）への送信もあったが、許可リストにないので遮断した。
遮断しても Claude Code の動作は止まらなかった。

**Codex の通信先と API キーの扱い**：
`codex exec` が読む環境変数は `OPENAI_API_KEY` ではなく `CODEX_API_KEY` である。
Codex は Responses API を WebSocket で呼ぼうとし（`GET /v1/responses` の upgrade）、失敗すると HTTPS に切り替える。
どちらも同じ規則で差し替えられた。
Codex は API キーのモードでも `chatgpt.com` にキーを送っていた（`/backend-api/plugins/featured` など）。
この宛先は OpenAI の規則に登録していないので、プロキシが遮断した。
OpenAI の API キーを `chatgpt.com` に送ってよいかは、規則として明示的に決める必要がある。

**DNS**：
ゲストは名前解決にランナーの dnsmasq を使う。
透過プロキシでは、ゲストが名前を解決できないと接続を始められないので、DNS は欠かせない。
今の dnsmasq はすべての問い合わせを上流へ転送しているので、DNS の問い合わせ名に情報を載せて持ち出す経路が残っている。
段階 4 では、許可リストにある名前だけに答える DNS をランナーに置く。

**VM のサブネット**：
最初は 10.0.0.0/30 を使ったが、開発機の LAN の DNS が 10.0.0.1 だったため、ランナーの dnsmasq が上流の DNS を自分自身とみなして無視した。
ランナーのコンテナは上流の DNS をホストから引き継ぐので、ランナーのサブネットはホストの環境と衝突しない範囲を選ぶ必要がある。
172.29.0.0/30 に変えたが、本番では設定で変えられるようにする。

**Docker Hub の取得先**：
イメージ本体は `production.cloudfront.docker.com` から取得される。
中継のみの宛先には `*.docker.com` を含める必要があった。

**内部サンドボックス**：
ゲストのカーネルは Landlock を有効にしており（`lsm=capability,landlock`）、一般ユーザーで bubblewrap がユーザー namespace を作れる。
Codex のサンドボックスは書き込みを拒否した。
VM の中でもツールの内部サンドボックスは無効にしなくてよい。

## 未確認の項目

- **サブスクリプションのログイン**：`codex login --device-auth` と Claude Code の `/login` がプロキシ越しの VM の中で完了するか。認証情報がないので確かめていない。
- **プレースホルダーの形式**：接頭辞を本物に似せたプレースホルダーは、Claude Code にも Codex にも手元で拒否されずに送信された。本物と形式が大きく違う値で同じことが成り立つかは確かめていない。
- **ストリーミング応答（SSE）が途中で切れないか**：認証エラーで止まるため、長い応答を流せていない。
- **スナップショット、discard、小さなファイルを大量に扱う処理の速度、Kubernetes の特権 Pod**：段階 2 の範囲外として残す。
