# 0018 — WebAssembly: 実行系を運ぶか、コードを吐くか

- 日付: 2026-08-13
- 状態: **調査完了・Path A を推奨（プロトタイプ実機動作確認済み・採択は未）**
- 動機: 公開の顔。README を読んだ人がリンクを踏み、その場で Xenith のプログラムを走らせる。
  「Xenith はブラウザで動く」を成立させたい
- 手法: 両案を**実際にビルドして**評価した。推測ではなく実測。Path A は最小の証明まで
  作り、native CLI との相互一致テストを通した（§6）
- 前提の確認: 0001 §3「実行性能の頂点を狙わない」は非目標のまま。0013 の教訓
  「無言の二重真実源」は本 RFC の中心的な判断材料

---

## 1. 問い

0001 のロードマップは WASM バックエンドを挙げていた。だが「ブラウザで動く」を
実現する道は2本あり、費用が2桁違う。

- **Path A — インタプリタを wasm32 に載せる**。コンパイラ＋VM をまるごと1個の
  wasm モジュールにして、JS がソース文字列を渡し stdout を受け取る。
  意味論は**1つのまま**（第二実装が存在しない）
- **Path B — WASM を出力対象にする**（Xenith → `.wasm`）。コード生成・値表現・
  トラップ算術・`Io.write` の import。**第二の意味論**が生まれ、以後永久に
  インタプリタと一致させ続ける義務を負う

「WASM 対応」という一語が両方を指すせいで、後者の値段が前者の値段に見える。
本文書はそれを分離する。

## 2. 実測（結論を決めたのはここ）

### 2-1. ワークスペースは wasm32 で**無改造で通る**

```console
$ rustup target add wasm32-unknown-unknown
$ cargo build -p xenith-vm -p xenith-driver --target wasm32-unknown-unknown --target-dir target-b3
   Compiling xenith-diag / xenith-syntax / xenith-sema / xenith-vm / xenith-driver
    Finished `dev` profile in 27.49s
```

**ソースの変更はゼロ**。`#[cfg(target_arch = "wasm32")]` を1つも足していない。
理由は依存関係が薄いから: 実行チェーンの外部依存は `serde` / `serde_json` だけで、
`wasm-bindgen` も `getrandom` も `js-sys` も入っていない。

### 2-2. 「壊れる」と予想したものの実地確認

| 予想された障害 | 実際 | 効いている理由 |
| --- | --- | --- |
| 0017 の `std::thread::scope` | **コンパイル通過** | wasm32 でも std に `thread` はある。`Builder::spawn` が実行時に `Err` を返すだけ |
| スレッドが取れない | **既に想定済みだった** | `Pool::start` は「ホストが1本もスレッドをくれない」= `None` を返し、`enter_main(.., None)` すなわち逐次実行器に落ちる。0017 §3 のフォールバックがそのまま効く |
| `std::env::var`（`XENITH_EXECUTOR`） | コンパイル通過・`Err(NotPresent)` | ただし既定が `Parallel` になるので、**wasm 側で明示的に `Sequential` を渡す**（§4） |
| `std::fs`（xenith-driver::project） | コンパイル通過 | 単一ファイル経路（`analyze_source`）は fs を触らない。プロジェクト探索は wasm では呼ばない |
| `std::process::ExitCode` | 影響なし | xenith-cli にしか無く、wasm はそれをリンクしない |
| `std::time` | 影響なし | xenith-bench にしか無い |
| 端末色 | **そもそも無い** | 診断レンダラは ANSI を一切使っていない（grep 0 件）。JSON wire がそのまま使える |

**0017 は wasm を意識せずに書かれたのに、wasm で必要な逃げ道を全部持っていた。**
逐次実行器を「差分オラクルだから残す」と決めた判断（0017 §5）が、そのまま
「スレッドの無いホスト向けの実行器」として再利用できた。これが Path A の値段を
ほぼゼロにしている。

### 2-3. 成果物の大きさと速さ（実機・Chrome）

| 指標 | 値 |
| --- | --- |
| `xenith_wasm.wasm`（release） | **749,541 B** |
| 同 gzip | **244,659 B**（約 239 KB — 中サイズの JPEG 1枚） |
| `instantiateStreaming` | **14.8 ms** |
| hello.xn（parse＋check＋run） | **0.4 ms** |
| 20万回ループ（`total = total + i`） | **140.5 ms** |
| 同じループを native release CLI で | 249 ms（プロセス起動込みの wall-clock） |
| 線形メモリ | 1.13 MB |

ブラウザの解釈実行が native CLI と同じ桁に収まっている。**遊び場としての性能は
論点にならない**——これは Path B の唯一の強い売り文句を先に潰す事実である。

## 3. Path B の値段（買えるものと、払うもの）

Path B を採るなら、**インタプリタが今まさに保証している以下すべてを、二本目の
実装で再現し、永久に一致させ続ける**ことになる。

### 3-1. 再実装が必要なもの（spec が規範として縛っているもの）

| 項目 | 出典 | wasm 生成で必要になる仕事 |
| --- | --- | --- |
| トラップ算術 | spec/04 §3 | wasm の `i64.add` は**溢れても黙る**。加減乗・単項マイナス・シフトの全経路に検査を挿入し、さらに**トラップに span を載せる**必要がある。wasm には既定で例外がないので、span 付きトラップは戻り値の Result 化か側チャネルで運ぶ |
| 値意味論＋COW | spec/04 §1・0017 §4 | List/Map/Struct/Enum の参照カウントと `make_mut` 相当を線形メモリ上に自作する。**ネストした集約の内側まで uniquify する**要件（0017 §4 の必須テスト）を生成コードで守る |
| Map の挿入順 | spec/07 §4（規範） | ハッシュ表ではなく順序保持の実装を runtime に持つ |
| 評価順 | spec/04 §2 | 左から右・レシーバ先行を生成コードで固定 |
| prelude | spec/07 | インタプリタが実装している **28 個**のメソッド（`concat` `split` `trim` `try_to_int` `checked_add` `fold` `filter` `sorted` `join` `has_key` …）を wasm ランタイムライブラリとして書き直す |
| クロージャ | spec/06・0014 | 捕獲環境の表現・CaptureSafe の実行時的帰結 |
| ホール | spec/04 §5 | 到達したら**名前付きの**トラップ。生成コードにホール名と span を埋める |
| 終了コード | spec/04 §5 | 0 / 1 / 2 / 101 の区別をホスト境界で再現 |
| `Io.write` | spec/03 §4 | import 1本。**ここだけは安い** |
| タスク | spec/04 §7-8 | 後述 |

### 3-2. 並列性は Path B でも買えない

「wasm ならスレッドが使える」は**静的ホスティングでは成り立たない**。
wasm threads は `SharedArrayBuffer` を要求し、それは
`COOP: same-origin` ＋ `COEP: require-corp` のヘッダを要求する。
GitHub Pages のような静的ホストは任意ヘッダを付けられない。

つまり **Path B に移っても、ブラウザ上の `spawn` はやはり逐次実行になる**。
0017 の並列性は「公開の顔」の文脈では Path A / Path B のどちらでも得られない。
Path B の追加費用に対する見返りからこの項目は消える。

### 3-3. 二重真実源の代価 — 0013 が既に払った授業料

0011 の監査は「MCP は常に単一ファイルモード」という**無言の二重真実源**を
発見し、0013 はそれを `ProjectSnapshot` 一本化で潰した。同じ構図が
Path B では**遥かに大きい面積で**再発する:

- 一致を測る装置が要る。0017 §5 が並列/逐次でやったのと同じ**相互一致テスト**を、
  インタプリタと生成コードの間で回し続ける必要がある
- 一致すべき軸は stdout・終了コード・**トラップの本文と span**・ホール名・
  Map 反復順・診断——つまり spec がほぼ全部
- 不一致が出たとき「どちらが正しいか」を毎回決める仕事が発生する。
  0013 の教訓は「真実源は1つにできるならする」であって、
  「2つを上手に運用する」ではない

**維持者は1人**（0001 §2-4「個人で完走可能」）。この義務は非目標に触れる。

### 3-4. Path B が本当に買うもの

正直に列挙する。どれも今の目的（公開の顔）には要らない。

- **配布可能な単体アーティファクト**: `.wasm` を他ホスト（Wasmtime・エッジ関数・
  ゲームエンジンのプラグイン）に埋める道。ただし現時点でその需要は文書化されていない
- **起動時の解析コストの消滅**: 生成物は既に検査済み。ただし §2-3 のとおり
  現状 0.4 ms で、問題になっていない
- **将来の最適化の土台**: バイトコード VM でも同じ土台が要る。
  xenith-vm の doc-comment が既に「バイトコード VM は後回しの最適化」と書いている——
  そのときも `run` インタフェースの背後で差し替える計画であり、
  **意味論の第二実装を作る計画ではない**

## 4. 判定

**Path A を採る。Path B は「公開の顔」の理由では着手しない。**

理由を短く:

1. **費用が実測でほぼゼロ**。ワークスペース無改造で wasm32 が通る（§2-1）
2. **意味論が1つのまま**。第二実装が無いので、一致させ続ける義務が発生しない（§3-3）
3. **Path B の売り文句が両方消える**。性能は非目標（0001 §3）かつ実測で問題なし
   （§2-3）、並列性は静的ホストでは Path B でも得られない（§3-2）
4. **逐次実行器を使うことは緩和ではない**。0017 §5 が逐次を並列の**差分オラクル**と
   定め、両実行器の stdout・終了コード・診断がバイト一致することを主ゲートにしている。
   ブラウザで走るのは「劣化版」ではなく、**プロジェクト自身のゲートが等価と保証した方**である

### 明示的に選んだこと

- wasm 側は `Executor::Sequential` を**名指しで**渡す。
  `Executor::from_env()` は wasm でも既定 `Parallel` を返し、その後
  `Pool::start` の失敗で逐次に落ちる——**結果は同じだが、事故的に正しいのは正しさではない**
- `wasm-bindgen` を使わない。生の `extern "C"` 3本＋線形メモリで足りる。
  ビルド手順は `cargo build` 1回のままで、npm もバンドラも増えない

## 5. 作ったもの

```
compiler/tools/xenith-wasm/
  Cargo.toml                 cdylib + rlib（同じコードをホストで cargo test にかける）
  src/lib.rs                 run_source(&str) -> RunResult ＋ 生の wasm ABI
  web/playground.html        単体で完結するページ（インライン CSS/JS・依存ゼロ）
  web/verify.mjs             wasm と native CLI の相互一致テスト
```

`src/lib.rs` は**何も再実装していない**。CLI の単一ファイル経路をそのままなぞる:

```rust
let analysis = xenith_driver::analyze_source(source);   // 診断があれば exit 2
let parsed = xenith_syntax::parse(source);
let (table, _) = xenith_sema::def::collect(&parsed.module);
let outcome = xenith_vm::run_with(&parsed.module, &table, Executor::Sequential);
```

診断は `xenith_driver::wire::file_diagnostics` の共有 wire 形式で返す
（0013 §2 と同じ線引き: 共有するのはレンダラではなく意味モデル）。

ABI は3本だけ:

| export | 意味 |
| --- | --- |
| `xn_alloc(len) -> ptr` | 呼び手が UTF-8 ソースを書き込む領域 |
| `xn_run(ptr, len) -> ptr` | 実行。戻りは LE `u32` 長＋その長さの UTF-8 JSON |
| `xn_free(ptr, len)` | 結果バッファの解放 |

## 6. 検証

### 6-1. 相互一致テスト（wasm ↔ native CLI）

```console
$ node verify.mjs
  ok    hello                 exit   0  stdout "Hello, world"
  ok    arithmetic and loops  exit   0  stdout "385"
  ok    tasks                 exit   0  stdout "6"
  ok    a trap                exit 101  stdout "before"
  ok    main returns Err      exit   1  stdout ""
  ok    an undeclared effect  exit   2  XN4001
  ok    an unknown name       exit   2  XN2002
  ok    a hole                exit 101  stdout ""

all 8 programs agree — wasm module and native compiler
```

比較しているもの: **終了コード**・**stdout のバイト**・トラップの
`行:桁: runtime error: 本文`（パスだけ構造上異なる）・拒否時の
`行:桁: severity[コード]: 本文` が native のレンダリングに現れること。

### 6-2. 実機のブラウザ（Chrome・`python -m http.server` 経由）

```
Hello, world
exit 0
```

```
6                                                   ← scope + spawn ×2
exit 0

before                                              ← 1 / 0
playground.xn:3:13: runtime error: division by zero
exit 101

playground.xn:2:5: error[XN4001]: this call uses {Io.write}, which `main` does not declare
  fix: declare `uses {Io.write}`
not run — fix the diagnostics first
exit 2
```

native CLI と同一。診断の `fix` までブラウザに届いている。

### 6-3. 既存ゲート

- `cargo test --workspace --exclude xenith-bench --target-dir target-b3` → **675 passed / 0 failed**
- `cargo clippy --workspace --exclude xenith-bench --all-targets --target-dir target-b3 -- -D warnings` → **0**
- `cargo clippy -p xenith-wasm --target wasm32-unknown-unknown -- -D warnings` → **0**

`--exclude xenith-bench` の理由は §8 に記録した（本 RFC と無関係の既存の赤）。

## 7. 次のスライス

1. **配置**: `web/playground.html` と `xenith_wasm.wasm` を同じディレクトリに置いて
   静的ホストに上げる。README からリンクする。`file://` は
   fetch も module import も塞ぐので HTTP 配信が要る（1行のサーバで足りる）
2. **CI に相互一致を載せる**: `verify.mjs` を CI ステップにする。
   wasm を「ビルドが通る」だけでなく「**native と一致する**」で守る。
   0017 §5 の主ゲートと同じ形
3. **`Executor::from_env` に wasm の分岐を1行**: 現状は事故的に正しい（§4）。
   `#[cfg(target_arch = "wasm32")] => Sequential` を足して意図を型にする
4. **サイズを詰めるなら** `opt-level = "z"` ＋ `panic = "abort"` の wasm 専用プロファイル。
   ただし 239 KB gzip は既に十分小さく、**急ぐ理由がない**
5. **例と `xenith goals` をページに載せる**: 遊び場の価値は「ホールを埋める」
   ワークフローを見せられること。診断の `fix` は既に届いている

## 8. 記録 — 本 RFC と無関係な既存の赤

調査中に `cargo clippy --workspace` / `cargo test --workspace` が落ちることを確認したが、
**原因は本 RFC の外**である:

- 未追跡ファイル `compiler/tools/xenith-bench/src/usage.rs` に 6 件の
  `unknown character escape: ' '`（生文字列にすべき箇所）。そこから
  `FENCED_TIERS` 未解決・`Condition::T7*` 非網羅・`tier6_tasks_only` 不在が波及
- `compiler/tools/xenith-bench/src/main.rs` も作業ツリーで未コミット変更あり
- `xenith-bench` は `xenith-wasm` に依存していない。
  ワークスペースにメンバを足しても他メンバのコンパイルは壊れない

つまり別セッションの作業中の状態。**触っていない。**
`--exclude xenith-bench` で残り全部が緑であることを示した（§6-3）。

## 9. 入れないもの

- **WASM バックエンド（Path B）**。上記の理由で、少なくとも
  「ブラウザで動かしたい」という動機では着手しない。
  着手するなら動機は別（他ホストへの埋め込み配布）で、
  そのときは 4AI 並列レビュー（feedback: 重い設計判断）を通すべき決定である。
  Path A は意味論を1つも足さないのでそのゲートを要しない
- **`wasm-bindgen` / npm / バンドラ**。3本の export で足りている
- **ブラウザ上の本物のスレッド**。静的ホストでは COOP/COEP を付けられない（§3-2）
- **プロジェクトモード（複数ファイル）のブラウザ対応**。単一ファイルで
  「リンクを踏んだら走る」は成立する。仮想 FS は必要になってから
- **wasm 版に独自の診断レンダラ**。JSON wire を返し、描画はページの仕事にした
  （0013 §2 と同じ線引き）
