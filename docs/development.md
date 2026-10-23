# 開発

クレートの構成は [README](../README.md#リポジトリの構成) に、設計の前提は [調査と計画](research-and-plan.md) に、段階ごとの計画と結果は [実装手順](implementation-steps.md) にある。

## ビルドと静的な確認

```sh
cargo build --workspace
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

`cargo test` は VM を使わずに数秒で終わる。
ゲストのバイナリ（`chloe-guest-agent`）は、`images/guest/rootfs/build.sh` が musl でビルドしてルートファイルシステムに入れる。

## Web の画面

Web の画面は `web/` にある。
構成、依存の向き、状態の置き場所、安全のための決まりは [Web の画面の方針](web.md) にある。
Node.js と pnpm は mise などで入れる（版は `web/package.json` の `packageManager`）。

```sh
cd web
pnpm install
pnpm dev            # http://localhost:5173 。/api はゲートウェイ (CHLOE_API、既定 http://127.0.0.1:8080) へ中継する
pnpm typecheck
pnpm lint           # oxlint
pnpm format         # oxfmt (確かめるだけなら pnpm format:check)
pnpm test           # Vitest
pnpm build          # web/dist に書き出す
```

開発では、ゲートウェイの `[api]` の `public_url` を Vite の URL（`http://localhost:5173`）にし、`web_root` は書かない。
GitHub のログインのコールバックも Vite を通ってゲートウェイに届き、cookie と `Origin` の確認もそのまま働く。
GitHub App を使わないときは、`chloe web login` が出す URL でログインする。

### 機能を足す

1. `web/src/features/<機能>/` を作り、`index.ts` と、要れば `feature.ts`（ナビゲーションの項目、出来事の処理）を書く。
2. `web/src/app/features.ts` の一覧に足す。
3. `web/src/routes/_app/<パス>.tsx` にルートを書く（ログインしたあとの画面は `_app` の下に置く）。

### API の型とテスト

管理 API を変えたら、スペックを書き出し直して型を作り直す。
`cargo test` は、`web/openapi.json` がコードとずれていると失敗する。

```sh
cargo run -p chloe-gateway -- openapi > web/openapi.json
(cd web && pnpm api)
```

テストの偽の API は `web/src/test/msw.ts` の `http` で書く。
スペックにないパスや、型の合わない応答は `pnpm typecheck` で落ちる。

### E2E

本物のゲートウェイと VM を相手にする画面の確認は `web/e2e` に Playwright で書き、`images/guest/dev/test-all.sh web` で回す（下の「VM を使う確認」）。
スクリプトがゲートウェイを起動し、`chloe web login` の URL（`CHLOE_E2E_LOGIN`）と画面の URL（`CHLOE_E2E_URL`）を渡して `pnpm e2e` を実行する。
初めは `pnpm exec playwright install chromium` でブラウザを入れる。
コンソールに誤り（CSP の違反など）が出ると、テストは落ちる。
`CHLOE_E2E_SCREENSHOTS=1` を付けると、すべてのテストの終わりの画面を `build/dev/web/playwright` に撮るので、見た目を確かめられる。

## 確認の二層

- **Rust のテスト**：判断の中身を確かめる。controller の停止の予約、作業中のエージェントを待つ停止、reconcile、問い合わせへの答え、方針の層の重ね方、相互 TLS の役割の判定などは、偽のランナーと偽の guest-agent、偽の GitHub を相手に確かめる。
- **VM を使う確認**：部品がつながっていることを確かめる。本物のゲートウェイ、`chloe-node`、ランナー、VM を動かし、機能ごとに代表の項目だけを確かめる。

判断の細かな場合分けは Rust のテストに足し、VM を使う確認には足さない。

## VM を使う確認

KVM と Docker が要る。
`systemd` の確認も Docker の特権コンテナで行うので、root の権限は要らない。
先にゲストのイメージをビルドしておく（[インストール](install.md#deb-を作る)）。
本物の API キーは使わず、偽の値をシークレットに登録して、差し替えた要求が宛先で 401 になることで確かめる。

```sh
images/guest/dev/test-all.sh            # すべてを並べて実行する
images/guest/dev/test-all.sh mvp node   # 名前を指定して実行する
```

| 確認 | 内容 |
| --- | --- |
| `smoke` | VM の起動、Docker、mise、起動し直してもワークスペースと Docker のデータが残ること |
| `net` | 透過プロキシ、許可リスト、DNS、差し替え、中継のみの宛先、mise がプロキシを通してツールを入れられること |
| `root` | ランナーが渡す環境変数、ルートの使い捨てと保持 |
| `disk` | 指定した大きさでディスクを作ること、次の起動で中身を残して広げること、縮めないこと |
| `mvp` | 二人の利用者、シークレット、動いたままの更新、HTTP の要求の記録、エージェントからの依頼、ワークスペース、停止、管理 API |
| `web` | Web の画面の E2E（`web/e2e`）。ビルドした画面を出すゲートウェイを相手に、Playwright の Chromium で、スマートフォンの幅と広い画面の両方で、端末、キーの列、Inbox で答えることを確かめる |
| `github` | GitHub App（GitHub のふりをするサーバー）、Device Flow を必須にしたログイン、共通のユーザー名と鍵の結び付け、二つの Organization、設定とシークレットの権限、管理の TUI |
| `lifecycle` | 作業中のエージェントを待つ停止、ゲートウェイの再起動、ポート転送、監査ログ |
| `machine` | ワークスペースごとの VM の vCPU の数とメモリ、止めるまでの時間（止めないことを含む） |
| `node` | 二つの `chloe-node`、参加、ほかのホストの HTTP の要求の記録、ディスクを広げること、相互 TLS、つながらないホスト |
| `systemd` | systemd で動かすランナー（`kind = "systemd"`）。systemd を PID 1 にした特権コンテナをホストの代わりにし、`deploy/` の unit、polkit の規則、ホストのネットワークの規則で、起動、利用者、外への通信と DNS、ホストとプライベートアドレスへの遮断、方針の入れ替え、ゲートウェイの再起動、停止と片づけを確かめる |

どの確認も VM の名前、状態のディレクトリ、ポートが別なので、並べて実行できる（2026-10-17 の時点で 320 項目が 79 秒）。
各スクリプトの出力は `build/dev/test-logs/<名前>.log` に出る。
各項目には直前の項目からの秒数が付き、ゲートウェイを通す確認は節ごとの所要時間を最後に出す。

確認のスクリプトを書くときは、次のようにする。

- 決まった秒数の `sleep` ではなく、条件（ログの行、ポート、ランナーの状態）を `wait_until` で待つ（`images/guest/dev/gateway-lib.sh`）。
- 停止までの時間と問い合わせの間隔を、確認と確認のあいだにランナーが止まらない程度に短くする。
- 本物の値（偽の値でも）とトークンを、確認の出力に出さない。

## deb の確認

`packaging/deb/build.sh` で作った deb を、systemd を PID 1 にした Ubuntu の特権コンテナに `apt` で入れて確かめる。
新しく入れる、ランナーを動かす、版を上げて入れ直す（動いているワークスペースが止まらない）、消す、purge する、の順に確かめる。
ゲストのイメージは `build/guest` を使う。

```sh
packaging/deb/build.sh
packaging/deb/test.sh 24.04
packaging/deb/test.sh 26.04
```

`build.sh` は lintian の結果も出す。
意図した警告は `packaging/deb/lintian-suppress` に理由とともに書く。

## VM に直接入る

ゲートウェイを通さずに、開発用のコンテナで VM を起動して入れる。

```sh
images/guest/dev/run.sh                                     # 端末でシリアルコンソールにつなぐ
CHLOE_DEV_DETACH=1 images/guest/dev/run.sh                  # バックグラウンドで起動する
target/debug/chloe-runner shell --vsock build/dev/vsock.sock
```

VM の中で `sudo reboot` すると、Firecracker が終わる。

## CI

GitHub Actions で、VM を使わずに回せる確認と deb のビルドを行う。
KVM が要る VM を使う確認と deb の確認（`packaging/deb/test.sh`）は、CI では回さず手元で回す。

- **CI**（`.github/workflows/ci.yml`）：main への push と Pull Request で、上の「ビルドと静的な確認」（clippy は `-D warnings`）と、Web の画面の lint、整形、テスト、ビルドを行う。`web/src/shared/api/schema.d.ts` と `web/src/routeTree.gen.ts` が、作り直したものと違うと落ちる。
- **deb**（`.github/workflows/deb.yml`）：main への push、`v` で始まるタグ、手動の実行で、`packaging/deb/build.sh` を実行し、`chloe-common`、`chloe-gateway`、`chloe-node` を成果物として置く。タグと手動の実行では、ゲストのカーネルとルートファイルシステムをビルドして `chloe-guest` も作る（手動の実行では外せる）。カーネルは、`images/guest/kernel/` が変わらなければキャッシュのものを使う。

## コミット

Conventional Commits の形で、説明は日本語で書く（`feat(gateway): ...`、`fix(controller): ...`、`docs: ...`）。
