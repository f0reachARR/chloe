# 方針の評価の調査

外への通信の方針に GraphQL の操作単位の規則を足すために、OpenShell の判定の仕組みを chloe に応用できるかを調べ、方針の評価の作り直しの案をまとめる。
調査日は 2026-11-03 である。
実装は後回しにし、この文書は調査の結果と採用する予定の案だけを残す。
OpenShell は `main` の 42671c2（2026-10-31）を読んだ。
OpenShell 全体の調査は [OpenShell の調査](research-openshell.md) にある。

## 結論の要約

- OpenShell の GraphQL の判定のうち、要求を解析して操作を取り出す部分は移植する価値がある。依存は crates.io にある `apollo-parser` と `serde_json` だけで、中心の関数は入出力を持たない。
- OpenShell のクレートをそのまま依存に入れることはできない。crates.io に出ておらず、OpenShell のほかのクレートと独自の要求の型に結び付いている。
- 今の方針（上から順に評価し、最初に一致した規則で決める）には、GraphQL の規則がうまく収まらない。互換性を崩してよいなら、拒否を優先し、判定の単位ごとに評価する方式に変える。
- 将来の拡張のために、評価には Rego を使う。管理者が書く規則は今までどおり TOML と API の構造化した形を既定にし、コントローラーがそれを Rego のモジュールに変換して、固定の Rego のライブラリと束ねる。
- 管理者は、必要に応じてカスタムの Rego で層の上下を無視して判定を上書きできる。ただし、上書きできる層は運用者が許したものに限り、宛先を増やすことと、Rust に残す検査を外すことはできない。

## 今の方針の評価

方針の規則（`crates/chloe-policy/src/path.rs`）は、宛先、メソッド、パスの組である。
規則を上から順に評価し、最初に一致した規則の許可か拒否で決め、どれにも一致しなければ遮断する。
個人、Organization、team、ワークスペースの層は、上の層の規則を先に並べて連結する（`crates/chloe-controller/src/runner.rs` の `BasePolicy::with_layers`）。
上の層の規則が先に一致するので、下の層は上の層の判定を覆せない。

この方式に GraphQL の規則を足すと、三つの問題が出る。

1. **バッチの要求**：GraphQL では一つの要求に複数の操作が入り、操作ごとに当たる規則が違うことがある。一つの要求に一つの規則を当てる方式では、操作 A を規則 1 で、操作 B を規則 2 で許すような要求を表せない。
2. **広い許可による素通り**：設定の例は `api.github.com` をホストごと許している（`images/guest/dev/gateway.example.toml`）。この規則が先に一致すると、`/graphql` への要求は GraphQL の規則まで届かない。
3. **下の層から締められないこと**：Organization が `api.github.com` を広く許していると、ワークスペースの層に足した拒否の規則は、上の許可が先に一致するので効かない。

本文の検査にも制限がある。
プロキシは、大きさが分かっていて 1MiB 以下の本文だけを読んで方針に渡し、それ以外は調べずに流す（`crates/chloe-egress/src/service.rs`）。
チャンクで送る本文や `content-length` のない HTTP/2 の本文は読まれないので、GraphQL の規則の宛先でこのまま通すと判定を素通りされる。

## OpenShell の GraphQL の判定

OpenShell の判定は、要求の解析と規則の評価の二つの層に分かれる。

**要求の解析**（`crates/openshell-supervisor-network/src/l7/graphql.rs`、本体は約 400 行）は次のことをする。

- POST の JSON（バッチの配列を含む）と、GET のクエリ文字列を読む。
- `apollo-parser` で問い合わせ文を構文解析する。再帰を 128 段、トークンを 20,000 個までに制限する。
- `operationName` で実行する操作を選ぶ。操作が複数あるのに名前がない要求は誤りにする。
- 操作ごとに、種類（query、mutation、subscription）、操作名、最上位のフィールドを取り出す。フラグメントも展開する。
- persisted query（ハッシュや ID だけを送る要求）を見分ける。
- 圧縮した本文と multipart の要求は断る。

変数（`variables`）は取り出さないので、引数による判定はできない。

**規則の評価**（`data/sandbox-policy.rego` 約 1,100 行、`src/opa.rs` 約 12,600 行）は、Rust の Rego 処理系 `regorus` で行う。
Rego のファイルは OpenShell に焼き込まれており、利用者が書く YAML の規則は Rego の `data` として渡される。
利用者が Rego を書く口はない。
GraphQL の規則は次のように書く。

```yaml
- host: api.github.com
  port: 443
  path: /graphql
  protocol: graphql
  enforcement: enforce
  rules:
    - allow:
        operation_type: query
    - allow:
        operation_type: mutation
        fields: [createIssue]
  deny_rules:
    - operation_type: mutation
      fields: [deleteRepository]
```

規則の意味は次のとおりである。

- 許可の規則は、最上位のフィールドがすべて一致したときだけ当たる。拒否の規則は、一つでも一致すれば当たる。
- バッチの要求は、中の操作が一つでも断られたら全体を断る。
- `protocol: graphql` を宣言したパスへの要求は、GraphQL の規則でしか許されない。同じホストの広い REST の規則では通らない。
- 解析に失敗した要求と、登録されていない persisted query は断る。
- `enforcement: audit` にすると、断る代わりに記録だけする。

## chloe に取り入れるもの

| 部分 | 扱い | 理由 |
| --- | --- | --- |
| クレートを依存に入れる | 入れない | crates.io になく、`openshell-core` などと独自の要求の型 `L7Request` に結び付いている |
| 解析の関数 | 移植する | Apache-2.0 なので出典を書けば取り込める。`L7Request` に依存するのは GET の解析だけで、クエリ文字列を渡す形に直せば済む |
| 規則の意味 | そのまま採る | バッチで規則を迂回されないための設計として妥当である |
| `protocol` の宣言による素通りの防止 | 採る | 上の問題 2 を、規則の順番に頼らずに防げる |
| `audit` の動作 | 採る | `gh` が送る変更の操作を記録で集めてから、断る方式に切り替えられる |
| Rego の評価 | 採る（形を変える） | 下の「採用する予定の案」に書く |

## 採用する予定の案

### 規則の源と、コンパイルの結果

規則の源は、今までどおりゲートウェイの設定の TOML と、API で保存する構造化した JSON（データベースの `policies` と `workspace_policies`）にする。
コントローラーはそれを Rego のモジュールに変換し、次の三つを束ねたもの（以下、**バンドル**）をプロキシに渡す。

| 部品 | 中身 | 作る者 |
| --- | --- | --- |
| ライブラリ | 照合と最終の判定（`data.chloe.egress.decision`） | chloe に焼き込む |
| 規則のモジュール | 層ごとの構造化した規則を、Rego のデータの直書きにしたもの | コントローラーが生成する |
| カスタムのモジュール | 管理者が書いた Rego | 管理者 |

規則一つ一つを Rego の判定文に書き下す形はとらない。
文字列を判定文に埋め込むとエスケープの漏れでコードを注入される余地ができるが、データの直書きだけならその余地はない。
また、Web と TUI のフォームは構造化した規則を編集しているので、源を構造化した形のまま残せばフォームを作り直さずに済む。

### 設定の形

```toml
[policy]
# 復号しない宛先は、規則から外して別の一覧にする（今は規則の action の一つ）
passthrough = ["example-pinned.com"]

# 宛先とパスごとに、本文をどの形式として読むかを宣言する。
# 宣言したパスへの要求は、その形式の条件を持つ規則でしか許されない。
[[policy.endpoints]]
hosts = ["api.github.com"]
paths = ["/graphql"]
protocol = "graphql"

[[policy.allow]]
name = "github-rest-read"
hosts = ["api.github.com"]
methods = ["GET"]

[[policy.allow]]
name = "gh-graphql-read"
hosts = ["api.github.com"]
graphql = { operation_type = "query" }

[[policy.allow]]
name = "gh-pr"
hosts = ["api.github.com"]
graphql = { operation_type = "mutation", fields = ["createPullRequest", "addComment"] }
mode = "audit"

[[policy.deny]]
name = "github-no-delete"
hosts = ["api.github.com"]
methods = ["DELETE"]
paths = ["/repos/**"]
```

### 評価の方式

要求を判定の単位に分ける。
ふつうの HTTP では要求が一つの単位で、GraphQL では操作ごとに一つの単位になる。
あとで MCP の JSON-RPC のメッセージや WebSocket のテキストフレームを、同じ単位として足せる。

単位ごとに、次の順に決める。

1. 拒否の規則に一つでも当たれば断る。
2. 拒否に当たらず、許可の規則のどれかに当たれば許す。
3. どれにも当たらなければ断る。

すべての単位が許されたときだけ、要求を送る。
規則の並び順は結果に影響せず、記録に残す規則の名前を選ぶときにだけ使う。
GraphQL の宛先で本文を読めない要求（1MiB を超える、大きさが分からない、圧縮されている）と、解析できない要求は断る。

### 層の重ね方

既定の層の重ね方では、拒否はどの層のものも必ず効く。
下の層は、上の層が許した範囲を締めることができる（今の方式ではできない）。
許可の規則には今の制約を残し、下の層は上の層の規則に出てこない宛先にしか許可を足せない。
上の層の規則に出てくる宛先は、その層の受け持ちとし、その層が許さなかった単位はその層が断ったものとみなす。
こうしないと、下の層が許可を足して上の層の範囲を広げられてしまう。

### カスタムの Rego と上書き

カスタムのモジュールは、層ごとに決まったパッケージ（例：`chloe.custom.org_fortefibre`）に、二つの集合を出せる。

- **`deny`**：断る理由を足す。既定の層の重ね方に従い、締めることにしか使えない。
- **`override`**：層の上下を無視して、単位の判定を許可か拒否に置き換える。上の層の拒否も覆せる。

```rego
package chloe.custom.org_fortefibre

import rego.v1

repos := {"R_kgDOxxxx", "R_kgDOyyyy"}

# 締める
deny contains "pr-to-unlisted-repo" if {
	some op in input.request.graphql.operations
	op.operation_type == "mutation"
	"createPullRequest" in op.fields
	not op.variables.input.repositoryId in repos
}

# 層を無視して許す
override contains {"unit": unit, "action": "allow", "reason": "release-bot"} if {
	some unit in data.chloe.egress.units
	unit.kind == "graphql"
	unit.op.operation_name == "CreateRelease"
}
```

`override` には次の制約を置く。

- 上書きを使えるのは、運用者の層と、運用者がゲートウェイの設定で許した Organization と team だけにする。許していない層の `override` は、保存するときに断る。どの Organization の管理者でも上書きできると、運用者が全体に掛けた拒否を Organization が外せてしまう。
- 同じ単位に複数の `override` が当たり、許可と拒否が食い違ったら拒否を採る。
- `override` で決まった判定は、記録に「どの層のどの理由で上書きしたか」を残す。

カスタムのモジュールは `decision` を定義できず、決まったパッケージに集合を出すことしかできない。
最終の判定はライブラリだけが組み立てる。
評価が失敗したとき（規則の衝突、実行時の誤り、時間切れ）は要求を断り、監査ログに残す。
`http.send` のような外とやり取りする組み込みの関数は無効にする。
regorus で評価の時間や手数に上限を掛けられるかは、まだ確かめていない。

### 上書きできないもの

次のものは Rust に残し、`override` でも外せない。

- **宛先を増やすこと**：VM の DNS で引ける名前（`reachable_hosts`）と、SNI を見て復号しないで中継するかの判定（`tls_decision`）は、HTTP の要求ができる前に決まる。要求ごとに評価する Rego は、ここに関われない。新しい宛先を足せるのは構造化した規則だけである。
- **接続と要求の検査**：SNI と Host の照合、パスの正規化、CA と依頼のための内部の名前（`ca.chloe.internal`、`ask.chloe.internal`）。
- **シークレット**：プレースホルダーを登録していない宛先へ送る要求の遮断、差し替え、SigV4 の署名し直し。

### 置き場所と流れ

1. **保存**：構造化した規則は今の表に置く。カスタムの Rego は、持ち主の層ごとに新しい表に置く（本文、ハッシュ、更新者、更新時刻）。
2. **保存するときの検査**（ゲートウェイの API）：regorus で構文を解析し、ライブラリと一緒に読み込む。決まった見本の入力を評価し、誤りなく判定が出ることを確かめる。決まったパッケージの外に規則を書いたモジュールと、許されていない層の `override` は断る。今の `check_rules` の役割は、この検査が引き継ぐ。
3. **コンパイル**（コントローラー）：層ごとの規則のモジュールとカスタムのモジュールを束ね、バンドルにハッシュを付ける。
4. **配布**：今の制御用のソケット（`crates/chloe-egress/src/control.rs`）で、`PolicyConfig` の代わりにバンドルを送る。プロキシを動かしたまま入れ替える仕組みはそのまま使える。
5. **評価**（プロキシ）：regorus を包む部分を `chloe-policy` に置き、`decide()` のうち規則を照合する部分だけを置き換える。
6. **記録**：断った記録と上書きした記録に、バンドルのハッシュと理由を残す。

手元の要求の見本をバンドルで評価する機能（`chloe policy eval request.json` や同じ働きの API）があると、管理者がカスタムの規則を調べやすい。

### Rego に渡す入力

カスタムの Rego が書けるようになると、`input.request` の形が管理者との約束になる。
`input.version` を付けて文書にする。

- **渡すもの**：ホスト、メソッド、正規化したパス、決まったヘッダー、GraphQL の操作（種類、操作名、最上位のフィールド、変数）。あとで MCP と WebSocket を足す。
- **渡さないもの**：本文そのもの、本物の値、プレースホルダーの値。

GraphQL の変数を渡しても、引数による判定は確実ではない。
問い合わせ文の中に値を直に書かれると、変数を見る規則は素通りされる。
これを防ぐには引数の解析まで Rust で行う必要がある。

## 移行の影響

評価の方式が変わるので、今の規則を自動では正しく変換できない。
今の方式では先に一致した許可があとの拒否を打ち消すが、新しい方式では拒否が勝つ。
保存してある規則はマイグレーションで変換し、前にある許可と重なる拒否を一覧にして管理者に確かめてもらう。

手を入れる範囲は次のとおりである。

- `chloe-policy`（`path.rs` は作り直す。regorus を包む部分を足す）
- `chloe-controller` の層の合成と、規則の出どころの表示
- `chloe-egress` の制御用のソケットと本文の読み込み
- ゲートウェイの API、TUI（`tui/rules.rs`、`tui/policy_form.rs`）
- Web の `features/policies`、OpenAPI、`web/src/shared/api/schema.d.ts`
- 設定の例と文書

Web には Rego を編集する欄が要る。
フロントエンドの基盤に関わるので、エディターの部品は実装の前に決める。

## 実装するときの段階の分け方

1. 評価の方式の変更（拒否を優先し、単位ごとに判定する）と、保存してある規則の変換
2. regorus による評価と、バンドルの配布
3. `endpoints` の宣言と GraphQL の条件
4. `mode = "audit"` と、`gh` が使う変更の操作の既定の一覧
5. カスタムの Rego の保存、検査、上書きの許可
6. Web と TUI の画面

## 未決事項

- regorus で評価の時間と手数に上限を掛けられるか。掛けられないなら、別のスレッドで評価して時間で打ち切るか。
- `gh` が使う GraphQL の変更の操作の一覧。`audit` で集めてから決める。
- GraphQL の変数を入力に渡すか。渡しても引数による判定は確実でないことを、文書でどう伝えるか。
- 保存してある規則を変換するか、管理者に作り直してもらうか。

## 参考資料

- [OpenShell のネットワークの規則](https://docs.nvidia.com/openshell/latest/how-it-works/policies/network-rules)
- [OpenShell の方針のスキーマ](https://docs.nvidia.com/openshell/latest/how-it-works/policies/schema)
- [regorus](https://github.com/microsoft/regorus)
- [apollo-parser](https://crates.io/crates/apollo-parser)
