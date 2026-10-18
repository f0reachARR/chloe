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

## 確認の二層

- **Rust のテスト**：判断の中身を確かめる。controller の停止の予約、作業中のエージェントを待つ停止、reconcile、問い合わせへの答え、方針の層の重ね方、相互 TLS の役割の判定などは、偽のランナーと偽の guest-agent、偽の GitHub を相手に確かめる。
- **VM を使う確認**：部品がつながっていることを確かめる。本物のゲートウェイ、`chloe-node`、ランナー、VM を動かし、機能ごとに代表の項目だけを確かめる。

判断の細かな場合分けは Rust のテストに足し、VM を使う確認には足さない。

## VM を使う確認

KVM と Docker が要る。
`systemd` の確認も Docker の特権コンテナで行うので、root の権限は要らない。
先にゲストのイメージをビルドしておく（[インストール](install.md#ビルド)）。
本物の API キーは使わず、偽の値をシークレットに登録して、差し替えた要求が宛先で 401 になることで確かめる。

```sh
images/guest/dev/test-all.sh            # すべてを並べて実行する
images/guest/dev/test-all.sh mvp node   # 名前を指定して実行する
```

| 確認 | 内容 |
| --- | --- |
| `smoke` | VM の起動、Docker、Claude Code と Codex、起動し直してもワークスペースと Docker のデータが残ること |
| `net` | 透過プロキシ、許可リスト、DNS、差し替え、中継のみの宛先、Claude Code と Codex が TLS の検証を通ること |
| `root` | ランナーが渡す環境変数、ルートの使い捨てと保持 |
| `disk` | 指定した大きさでディスクを作ること、次の起動で中身を残して広げること、縮めないこと |
| `mvp` | 二人の利用者、シークレット、動いたままの更新、ワークスペース、停止、管理 API |
| `github` | GitHub App（GitHub のふりをするサーバー）、Device Flow を必須にしたログイン、共通のユーザー名と鍵の結び付け、二つの Organization、設定とシークレットの権限、管理の TUI |
| `lifecycle` | 作業中のエージェントを待つ停止、ゲートウェイの再起動、ポート転送、監査ログ |
| `node` | 二つの `chloe-node`、参加、ディスクを広げること、相互 TLS、つながらないホスト |
| `systemd` | systemd で動かすランナー（`kind = "systemd"`）。systemd を PID 1 にした特権コンテナをホストの代わりにし、`deploy/` の unit、polkit の規則、ホストのネットワークの規則で、起動、利用者、外への通信と DNS、ホストとプライベートアドレスへの遮断、方針の入れ替え、ゲートウェイの再起動、停止と片づけを確かめる |

どの確認も VM の名前、状態のディレクトリ、ポートが別なので、並べて実行できる（2026-10-17 の時点で 320 項目が 79 秒）。
各スクリプトの出力は `build/dev/test-logs/<名前>.log` に出る。
各項目には直前の項目からの秒数が付き、ゲートウェイを通す確認は節ごとの所要時間を最後に出す。

確認のスクリプトを書くときは、次のようにする。

- 決まった秒数の `sleep` ではなく、条件（ログの行、ポート、ランナーの状態）を `wait_until` で待つ（`images/guest/dev/gateway-lib.sh`）。
- 停止までの時間と問い合わせの間隔を、確認と確認のあいだにランナーが止まらない程度に短くする。
- 本物の値（偽の値でも）とトークンを、確認の出力に出さない。

## VM に直接入る

ゲートウェイを通さずに、開発用のコンテナで VM を起動して入れる。

```sh
images/guest/dev/run.sh                                     # 端末でシリアルコンソールにつなぐ
CHLOE_DEV_DETACH=1 images/guest/dev/run.sh                  # バックグラウンドで起動する
target/debug/chloe-runner shell --vsock build/dev/vsock.sock
```

VM の中で `sudo reboot` すると、Firecracker が終わる。

## コミット

Conventional Commits の形で、説明は日本語で書く（`feat(gateway): ...`、`fix(controller): ...`、`docs: ...`）。
