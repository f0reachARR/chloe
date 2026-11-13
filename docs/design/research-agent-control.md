# エージェントを外から操作する方法の調査

VM の中の Claude Code や Codex を、端末の外から操作する方法を調べる。
知りたいことは二つある。
一つは、エージェントが組み込みで持つ質問（Claude Code の AskUserQuestion、Codex の `request_user_input`）と権限の確認を、chloe の依頼（[段階 18](implementation-steps.md#段階-18エージェントからの依頼通信の許可と質問)）に置き換える方法である。
もう一つは、TUI の代わりに、チャットのような Web の画面からエージェントを動かす方法である。

調査日は 2026-10-23 である。
対象の版は、Claude Code が v2.1.309（`@anthropic-ai/claude-agent-sdk` は 0.3.309）、Codex が `rust-v0.164.0`（openai/codex の `ca9d103`）、Ovis が `8bd6480` である。
どれも版ごとに変わるので、取り入れるときは、その時点の文書とソースで確かめ直す。

## 結論の要約

- **TUI のまま、質問と権限の確認を依頼に回す**：フックでできる。Claude Code は、`PreToolUse`（AskUserQuestion）と `PermissionRequest` のフックが答えを返せる。Codex は、`PermissionRequest` のフックで権限の確認に答えられる。ただし Codex 自身の質問に答えるフックはない。
- **Codex の質問**：`codex app-server` のデーモンにつなげば受け取れる。TUI も同じデーモンにつなぐので、TUI と chloe のどちらで答えてもよく、先に答えた側が採られる。
- **チャットの画面**：エージェントを headless で動かす方法（Claude Agent SDK、Codex app-server、ACP）は情報が多いが、Claude Code では TUI と同時に使えない。TUI を残したまま、記録ファイルを読んでチャットとして描き、入力は端末に打ち込む方法（Ovis）なら、同時に使える。
- **chloe に合う組み合わせ**：表示は記録ファイルを読み、入力はエージェントの端末に打ち込み、質問と権限の確認はフックで依頼に回す。このためには、接続が切れてもエージェントの端末を持ち続ける仕組みが VM の中に要る。
- **使わないもの**：Anthropic の Remote Control と Channels、Codex の remote-control は、claude.ai や ChatGPT の画面と中継に結び付いており、chloe の画面からはつなげない。端末の画面を読み取って操作する方法は、エージェントの版が変わるたびに直すことになる。

## 方法の分類

調べたツールがエージェントとつなぐ方法は、次のように分けられる。

| 方法 | 使っている例 | TUI と同時に使えるか | 性質 |
| --- | --- | --- | --- |
| 端末の画面を読み取る | Coder AgentAPI、Ovis のカード | 使える | エージェントの見た目が変わると壊れる |
| フック | 承認を Telegram などに送るツール | 使える | 質問と権限の確認には足りるが、会話の本文は取れない |
| headless の公式の入口（Claude Agent SDK、Codex app-server） | T3 Code、claudecodeui、Happy | Claude Code は使えない。Codex は使える | 取れる情報がもっとも多い。エージェントごとに実装が要る |
| ACP のアダプター | Zed、Happy、モバイルのクライアント | 使えない | 一つのクライアントで Claude Code と Codex を動かせる |
| 記録ファイルを読む | Ovis、Happy のローカルモード、claudecodeui | 使える | 表示専用。形式は公開された仕様ではない |
| 承認をやめる | Terragon、opcode | 該当なし | サンドボックスを安全の境界にする。質問は残る |
| ベンダーの中継 | Claude Code の Remote Control、Codex の remote-control | 使える | ベンダーの画面専用で、自前の画面からはつなげない |

## Claude Code

### フック

公式の文書は <https://code.claude.com/docs/en/hooks> にある。

**AskUserQuestion に答える**：`PreToolUse` のフックを matcher `AskUserQuestion` で置き、`permissionDecision: "allow"` と、答えを入れた `updatedInput` を返す。
文書には、AskUserQuestion と ExitPlanMode では `allow` だけでは答えにならないと書かれている。
`updatedInput` は入力の全体を置き換えるので、`questions` も返し直す。

```json
{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow",
  "updatedInput": {"questions": [...], "answers": {"Which framework?": "React"}}}}
```

AskUserQuestion の入力は、1 から 4 個の質問（`question`、12 文字までの `header`、2 から 4 個の `options`、`multiSelect`）である。
`answers` は質問の文から選んだラベルへの対応で、複数を選んだときはラベルをカンマでつなぐ。
今の chloe の質問は一問と選択肢だけなので、取り込むには依頼の形を広げる必要がある。

**権限の確認に答える**：`PermissionRequest` のフックは、入力に `tool_name`、`tool_input`、`permission_suggestions` などを受け、`hookSpecificOutput.decision` に `behavior`（`allow` か `deny`）を返す。
`allow` では `updatedInput` と `updatedPermissions` を、`deny` では `message` と `interrupt` を付けられる。
一致する deny の規則は、フックの `allow` でも上書きできない。

**フックの種類と待つ時間**：フックには `command`、`http`、`mcp_tool`、`prompt`、`agent` の種類がある。
`command`、`http`、`mcp_tool` は既定で 600 秒まで待ち、`timeout` で延ばせる。
上限は文書に書かれていない。
chloe の VM からコントロールプレーンへの問い合わせは短い往復に限っているので（`ask.chloe.internal` は 10 秒）、`http` のフックで長く待つことはできない。
`chloe-ask` を `command` のフックとして呼び、その中で待つ形になる。

**端末のダイアログとの関係**：TUI では、フックが待っているあいだも端末のダイアログが出て、先に答えた側が採られる。
Issue の報告では、端末で答えたあともフックのプロセスはタイムアウトまで残る（v2.1.211 での報告で、公式の文書には書かれていない）[^issues]。
端末で答えられたことを `PostToolUse` などで知り、依頼を取り下げる仕組みが要る。

[^issues]: <https://claudeissues.com/issue/79651-feature-hook-event-when-a-permission-request-is-resolved-permissionresult>、<https://claudeissues.com/issue/15433-bug-permissionrequest-hook-process-not-terminated-on-terminal-response>

ほかに使えそうなフックを並べる。

- **Elicitation**：MCP のサーバーからの elicitation に、フックが `action`（`accept`、`decline`、`cancel`）と `content` で答えられる。答えるとダイアログは出ない。
- **Notification**：`permission_prompt`、`idle_prompt`、`elicitation_dialog` などで発火する。答えることはできず、知らせるだけである。
- **SessionStart**：入力に `session_id` と `transcript_path` が入る。端末とセッションの記録を対応づけるのに使える（Happy はこの方法である）。
- **Stop**：入力に `last_assistant_message` が入る。

### headless と Agent SDK

Agent SDK は、`claude` を次の引数で子プロセスとして起動し、標準入出力で JSON を一行ずつやりとりする。

```text
claude --output-format stream-json --verbose --input-format stream-json --permission-prompt-tool stdio [--resume <id>] ...
```

権限の確認と AskUserQuestion は、どちらも `control_request`（`subtype: "can_use_tool"`）として出てくる。
SDK では `canUseTool` のコールバックにあたり、文書には、答えるまで無期限に待ってよいと書かれている[^sdk]。
状態は `system/session_state_changed`（`idle`、`running`、`requires_action`）で分かる。
ただし、この一行ずつのやりとりは公式の文書で仕様として定められておらず、SDK の内部の約束である。
自前の画面から使うなら、SDK を間に置くほうが版の変化に強い。

[^sdk]: <https://code.claude.com/docs/en/agent-sdk/user-input>

`--permission-prompt-tool mcp__<server>__<tool>` は `-p` でしか効かず、TUI と一緒には使えない。
headless で動かしたセッションは、TUI で `claude --resume <id>` として開き直せる。
同時に両方から触ることはできないので、TUI とチャットを切り替える形になる。

### Anthropic の遠隔の機能

**Remote Control**：手元の Claude Code を、claude.ai/code と Claude のアプリから操作する。
権限の確認と AskUserQuestion にも遠隔から答えられる。
claude.ai のサブスクリプションでのログインが要り、API キーでは使えない。
つなげる画面は claude.ai と Claude のアプリだけで、自前の画面からつなぐ口は公開されていない。

**Channels**（研究プレビュー）：MCP のサーバーが、動いているセッションにメッセージを送り込む仕組みである。
`claude/channel/permission` を宣言すると、権限の確認（Bash、Write、Edit など）も受けて答えられ、端末と先に答えた側が採られる。
AskUserQuestion は中継の対象として書かれていない。
claude.ai か Console での認証が要り、自作のチャネルは `--dangerously-load-development-channels` で読み込む。

どちらも、プロキシで API キーを差し替える chloe の作りとは合いにくい。

### 記録ファイル

セッションの記録は `~/.claude/projects/<作業ディレクトリから作った名前>/<セッションの ID>.jsonl` に残る。
文書には、形式は内部のもので版ごとに変わると書かれている[^sessions]。
チャットの表示には使えるが、形式が変わったときに直す前提で使う。

[^sessions]: <https://code.claude.com/docs/en/sessions>

## Codex

### app-server

`codex app-server` は、VS Code の拡張と Codex のアプリが使う JSON-RPC のサーバーである[^appserver]。
待ち受けは `stdio://`、`unix://`、`ws://IP:PORT` から選ぶ。
公式の文書は、app-server と WebSocket の待ち受けを experimental としている。
`codex app-server generate-ts` と `generate-json-schema` で、入れた版に合う型を作れる。

[^appserver]: <https://learn.chatgpt.com/docs/app-server>

クライアントは `initialize` のあと、`thread/start`、`thread/resume`、`turn/start`、`turn/steer`、`turn/interrupt` などでスレッドを操作する。
表示には `item/started`、`item/completed`、`item/agentMessage/delta` などの通知と、`thread/read` を使う。

権限の確認と質問は、サーバーからクライアントへの要求として届く。

| 要求 | 答え |
| --- | --- |
| `item/commandExecution/requestApproval` | `accept`、`acceptForSession`、`decline`、`cancel` など |
| `item/fileChange/requestApproval` | 同上 |
| `item/tool/requestUserInput`（experimental） | 質問の ID ごとの答え |
| `mcpServer/elicitation/request` | `accept`、`decline`、`cancel` と内容 |

Codex 自身の質問（`request_user_input`）は、既定では Plan モードでしか使えない。

### デーモンと TUI

`codex app-server daemon start` で動かしたデーモンがあると、TUI は起動時にそのソケット（`$CODEX_HOME/app-server-control/app-server-control.sock`）につなぐ。
別のクライアントが同じソケットにつないで `thread/resume` すれば、TUI と同じスレッドの通知、権限の確認、質問を受け取れる。
ソースで確かめた動きは次のとおりである。

- **全員に届く**：要求は、そのスレッドを購読している接続のすべてに届き、最初の答えだけが採られる。
- **取り下げ**：答えが採られると `serverRequest/resolved` が流れ、TUI は自分のプロンプトを閉じる。
- **送り直し**：あとから `thread/resume` した接続には、まだ答えていない要求が送り直される。
- **誤りも答えになる**：JSON-RPC の誤りで応えても、最初の答えとして扱われる。知らない要求にすぐ誤りを返すクライアントがいると、TUI の利用者から答える機会を奪う。

### フックとほかの入口

Codex のフックは、Claude Code とほぼ同じ形で `~/.codex/hooks.json` などに書く[^codexhooks]。
`PermissionRequest` のフックは、`decision.behavior` に `allow` か `deny` を返して権限の確認に答えられる。
種類は `command` と `mcp_tool` だけで、`http` はない。
`updatedInput` は返せない。
`request_user_input` に答えるフックはなく、`PreToolUse` で止めて理由の文に答えを書く手しかない（モデルがそれを答えと読むかは確かめていない）。

[^codexhooks]: <https://learn.chatgpt.com/docs/hooks>

`codex exec --json` は、権限の確認と質問をすべて断り、elicitation を取り消すので、人に答えを求める用途には使えない。
`codex mcp-server` は削除された（openai/codex の PR #42993）。
セッションの記録は `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` に残るが、圧縮されることがあり、データベースへの移行も進んでいる。
表示には app-server の `thread/read` などを使うほうが確かである。

## ACP

ACP（Agent Client Protocol）は、Zed が始めた、エディターとエージェントのあいだの JSON-RPC のプロトコルである。
権限の確認は `session/request_permission`、質問は `elicitation/create` で表す。
Claude Code と Codex には、それぞれアダプターがある。

- **`claude-agent-acp`**：Agent SDK の上に作られ、`canUseTool` を `session/request_permission` に、AskUserQuestion を elicitation に変える。
- **`codex-acp`**：app-server を自分で起動して、ACP に変える。

ACP のクライアントを一つ書けば、両方のエージェントを同じ画面で動かせる。
ただし、どちらのアダプターもエージェントを headless で持つので、TUI と同時には使えない。
`codex-acp` が自分で app-server を起動するので、Codex でもデーモンを通して TUI とつなぐ形にならない可能性が高い（確かめていない）。
HTTP と WebSocket での通信路は RFD が出ている段階で、SDK はまだない。
chloe なら、guest-agent で標準入出力を中継できるので、この点では困らない。

## ほかのツール

| ツール | つなぎ方 | 権限の確認と質問 | 状態 |
| --- | --- | --- | --- |
| Happy | Claude Code は、手元では端末と記録ファイル、遠隔では Agent SDK に切り替える。Codex は app-server | 遠隔では `canUseTool` | MIT、活発 |
| T3 Code | Claude Code は Agent SDK、Codex は app-server、ほかは各社の SDK か ACP | 各エージェントの API | MIT、初期 |
| claudecodeui | Claude Code は Agent SDK、Codex は app-server。履歴は記録ファイルから | Web で答える | AGPL-3.0、活発 |
| Vibe Kanban | Claude Code の stream-json を Rust で自前で話す | 画面で答える | Apache-2.0、会社が閉じてコミュニティが保守 |
| opencode | `opencode serve` が OpenAPI の HTTP と SSE を出す | `POST /session/:id/permissions/:id` | MIT（opencode 自身だけ） |
| Coder AgentAPI | メモリの中の端末でエージェントを動かし、画面の差分からメッセージを切り出す | 画面を読むしかない | MIT、保守終了 |
| Terragon、opcode | `-p` と `--dangerously-skip-permissions`、`codex exec` | 承認をやめる | 停止または保守のみ |

Conductor、Sculptor、Nimbalyst、Omnara Remote は、つなぎ方を確かめられなかった。

## Ovis

[Ovis](https://github.com/devswha/ovis) は、TUI と Web の画面を同時に使える点で、chloe が欲しいものにもっとも近い。
Ovis は、端末の多重化ツール [herdr](https://github.com/herdrdev/herdr) のプラグインであり、フックも SDK も app-server も使わない。
エージェントはふつうの TUI のまま herdr のペインで動き、Ovis はその上で三つのことをする。

**表示**：エージェントの記録ファイル（Claude Code の `~/.claude/projects/*.jsonl`、Codex の rollout のファイルなど）を読み、チャットとして描く。
難しいのは、ペインとセッションの対応づけである。
herdr が教えるセッションを使い、なければプロセスが開いているファイルを調べ、それでもなければ画面の文と記録の文を照らし合わせる。
最後の方法は表示にだけ使い、入力の送り先は決めない。
対応が付かなければ、チャットにせず端末を見せる。

**質問と権限の確認**：エージェントごとの画面の読み取り（`server/dialogs/`）で、端末に出たダイアログをカードにする。
答えは矢印と Enter のキー入力に変えてペインに送る。
送る前に画面を読み直し、カードにしたダイアログがまだ出ていて、カーソルが思った行にあることを確かめる（`server/dialogs/interaction-plan.ts`）。
読み取れないダイアログは、端末で答えるように案内する。

**入力**：チャットで書いた文は、ペインの入力欄に文字として送る。
作業中に送った文は、次のターンに回せる。

端末は herdr が持ち続けるので、手元の TUI、ブラウザ、スマートフォンが同じペインに同時につながる。
画面の読み取りは、エージェントの版ごとに正規表現を直し続ける作りである。

## chloe への当てはめ

### 質問と権限の確認を依頼に回す

VM のイメージは chloe が配っているので、作業中を知らせるフックと同じ場所（`/etc/claude-code/managed-settings.d/`、`/etc/codex/hooks.json`）に、最初からフックを置ける。
画面を読み取る Ovis の方法より、フックのほうが版の変化に強い。

- **Claude Code**：`PreToolUse`（AskUserQuestion）と `PermissionRequest` のフックで `chloe-ask` を呼び、依頼の答えを返す。
- **Codex の権限の確認**：`PermissionRequest` のフックで同じように返す。判定の JSON は Claude Code とほぼ同じ形である。
- **Codex の質問**：VM の中で app-server のデーモンを動かし、chloe の中継が同じソケットにつなぐ。知らない要求に誤りを返さないようにする。
- **依頼の形**：複数の質問、複数の選択、`header` を持てるようにし、権限の確認の種類（ツールの名前と入力）を足す。
- **取り下げ**：端末で先に答えられたら、依頼を取り下げる。Ovis の「送る前に、まだそのダイアログが出ているかを確かめる」考え方は、ここでも使える。

### チャットの画面

Ovis と同じく、表示は記録ファイルを読み、入力はエージェントの端末に打ち込む。
質問と権限の確認は、上のフックで依頼に回したものを、チャットの中にも出す。
こうすれば、Claude Code でも TUI とチャットを同時に使える。

足りないのは、エージェントの端末を持ち続ける仕組みである。
今の chloe では、SSH や Web の端末が切れるとシェルが終わる（[段階 22](implementation-steps.md#段階-22web-の端末)）。
VM の中で tmux を使うか、guest-agent が PTY を持ち続けるかして、SSH、Web の端末、チャットが同じエージェントの端末につながれるようにする必要がある。
端末とセッションの記録の対応は、`SessionStart` のフックで `session_id` と `transcript_path` を受け取れば、Ovis のように推し量らずに済む。

弱いのは二つである。
記録ファイルの形式は公開された仕様ではなく、版が上がると読み方を直すことになる。
チャットからの入力はキー入力なので、入力欄に書きかけの文があるときなど、端末の状態に左右される。

headless の方法（Agent SDK、app-server、ACP）は、TUI と同時に使えないことを受け入れるなら、取れる情報がもっとも多い。
Codex だけは、app-server のデーモンを通せば TUI と同時に使える。
VM が止まるとプロセスは終わるので、どの方法でも、セッションを `--resume` や `thread/resume` で開き直せることと、セッションを持つプロセスを作業中として数えることが前提になる。

## 確かめていないこと

- Claude Code の TUI で、`PreToolUse` が `allow` と `updatedInput` を返したときに、ダイアログを出さずに答えとして通るか。
- Claude Code のフックの待つ時間に上限があるか。
- Codex の `PermissionRequest` のフックが、app-server を通したときにも発火するか。
- Codex の app-server を実際に起動しての動き。ここに書いたことは、ソースと文書から読んだものである。
- `codex-acp` と app-server のデーモンを一緒に使えるか。
- Conductor、Sculptor、Nimbalyst、Omnara Remote のつなぎ方。
