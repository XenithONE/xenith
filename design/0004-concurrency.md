# 0004 — 並行性モデル

- 日付: 2026-08-01
- 状態: **採択**
- 経緯: 0002 §8 で grok が「**並行性はFFIより大きな設計負債。明示的async＋構造化並行性を
  早期に決めないと必ず書き直しになる**」と指摘した。当初の草案10項目に並行性の記述が
  **皆無**だったため、単独の決定事項として切り出した。
- 手法: 4モデル並列レビュー第2ラウンド（`codex` / `grok` / `agy` / `opencode`）。

---

## 1. 決定：構造化された stackless async/await（1モデル・2実行器）

**投票 3対1。** codex・grok・opencode が構造化 async/await、agy のみ色なしスタックフルファイバ。

言語モデルは**1つ**。実行器（スケジューリング方針）だけが**2つ**。

| 用途 | 実行器 | 方針 |
|---|---|---|
| サーバー | `IoExecutor` | リアクタ駆動のワークスティーリング。数万の休止接続がOSスレッドを消費しない |
| ゲーム | `FrameExecutor` | 固定ワーカー数・事前確保の有界キュー・暗黙確保なし・フレーム境界で барьер。フレームスライス中はスティールしない |

**これは2言語ではない。** 両者とも同一のアフィンタスク・スコープ・join ハンドル・
チャネル・キャンセル・エラー規則の上で動く。違うのは配置方針だけ。
アクタとジョブグラフは、このプリミティブ上の**ライブラリ**として提供する（言語機能にしない）。

**非構造的（detached）な spawn は提供しない。** 子タスクは親スコープを超えて生存できない。
サーバーの最上位サービススコープはプロセス終了まで生きてよい。

### なぜ緑スレッド（agy案）を採らなかったか

agy の主張（関数の色分けはLLM最頻出バグの1つ）は正しい。しかし2点で覆る。

1. **転移が決定的に大きい**（grok）。async/await は TypeScript・JavaScript・Python・C# の
   巨大な訓練コーパスを持つ。0002 §4 で採択した「構文的同型性／負の転移」原則を
   並行性に適用すると、ここは async/await 一択になる。
2. **色分け問題は本言語では静かに失敗しない。** `.await` の付け忘れは
   「`Task<T>` が来たが `T` が期待されている」という型エラーになり、
   さらに型付きホールが `expected: Task<T>` を直接返す（0002 §1）。
   agy が懸念した「同期文脈から非同期を呼ぶ」は**大声の型エラー**であって、静かな誤りではない。
3. 加えて grok は緑スレッドを「**訓練データ中で最も競合が多く、"テストは通るが壊れている"の温床**」
   として6候補中5位に置いた。ゲーム側でもフレームジッタの原因になる。

なお **`.await` を明示させること自体に価値がある**（opencode）。
中断点が構文に現れるので、フレーム予算の予測可能性が保たれる。暗黙のyieldは持たない。

---

## 2. 効果システムとの接続

**`async` は効果では「ない」。** 4モデル中3モデルが明示的に同意。
`async` は呼び出し規約／状態機械変換であり、`async fn f() -> T` ≅ `fn f() -> Task<T>`。
効果は future が await または spawn されるまで**潜在**する。

**`Task.spawn` は効果である。** spawn は並行な仕事を作り、キャンセルと資源所有を変えるため。
spawn には **capability（`Scope`）と効果（`Task.spawn`）の両方**が要る。

```xenith
async fn fetch_pair(
    tasks: Tasks,
    http: Shared<Http>,
    left: Url,
    right: Url,
) -> Result<(Bytes, Bytes), FetchError>
uses {Task.spawn, Http.get} {
    task_scope(executor: tasks.io, capacity: 2) as scope {
        let a = scope.spawn(job: async move || { http.get(url: left).await })?;
        let b = scope.spawn(job: async move || { http.get(url: right).await })?;
        Ok((a.await()?, b.await()?))
    }
}
```

規則：

- **子の効果集合 ⊆ spawn 地点で許された効果集合。** 違反はコンパイルエラー。
- `await` 自体は効果を追加しない。
- ブロッキング操作は `BlockingExecutor` capability と `Task.blocking` 効果を要求し、
  `IoExecutor` 上のタスクでは**拒否**される（イベントループの巻き添えを型で防ぐ）。
- `Scope` は**アフィンかつ脱出不能**。返却・ヒープ格納・子タスクへのムーブができない。

ゲームのフレーム更新は、効果集合が空であることを署名で証明できる：

```xenith
fn tick(world: World) -> World uses {} {
    // IO も spawn もできないことがコンパイラに保証されている
}
```

---

## 3. 借用チェッカ無しでのデータ競合排除（最重要）

> 「借用チェッカは要らない。要るのは **`Transfer`/`ShareSafe` と、`Shared` の中に非同期化された
> 内部可変性を入れさせないこと**。競合の話はそれで全部だ。」（grok）

アフィン所有権は**排他値**のエイリアシングを既に潰している。穴は `Shared<T>` と並行性の交点だけ。

### 3-1. 2つのマーカー

コンパイラ組み込みで**自動導出**される。

| マーカー | 意味 |
|---|---|
| `Transfer` | この値はタスク間を**移動**してよい |
| `ShareSafe` | この値は**並行に観測**されてよい |

**利用者はこれらを実装できない。** 監査済みの標準ライブラリ／FFI の `unsafe` のみが付与できる。
これは Rust より厳しい（Rust は `unsafe impl Send` を利用者に許す）。
個人維持者にとって、健全性の責任範囲を標準ライブラリ内に閉じ込められることが決定的に重要。

### 3-2. spawn 境界の規則

spawn されるクロージャは `move` でなければならず、捕獲される各値は**次のいずれか**：

```
T           where T: Transfer        // 親は所有権を失う
Shared<T>   where T: ShareSafe       // 同期済み、または読み取り専用の共有
```

**決して転送できないも**の: ローカル参照／ビュー、生ポインタ、アリーナ由来の借用ハンドル、
ロックガード、スレッド固定の capability、非同期化された内部可変値。

### 3-3. `Shared<T>` は可変操作を一切持たない

`Shared<T>` の原子参照数は**生存期間のみ**を保護する。構築は `T` を消費し、元の可変所有者を消す。
共有された可変状態は、コンパイラが知っているプリミティブ経由でしか作れない：

```
Shared<Mutex<T>>       T: Transfer のとき ShareSafe
Shared<Atomic*>        常に ShareSafe
Sender<T> / Receiver<T>    T: Transfer
```

`ShareSafe` が自動導出されるのは、**深く不変な集約**と上記の同期プリミティブのみ。

```xenith
// OK — 排他値を子へムーブ（アフィン）
scope.spawn(job: async move || { consume(buffer: owned_buf) })?;

// OK — 不変共有
let cfg: Shared<Config> = Shared.new(value: config);   // Config は深く不変 → ShareSafe
scope.spawn(job: async move || { render(config: cfg.share()) })?;

// OK — 明示的な共有可変
let counter: Shared<Mutex<Int>> = Shared.new(value: Mutex.new(value: 0));
scope.spawn(job: async move || { counter.lock().increment() })?;

// 拒否 — 非同期の内部可変性
let bad: Shared<Cell<Int>> = Shared.new(value: Cell.new(value: 0));
scope.spawn(job: async move || { bad.set(value: 1) })?;
// error: Cell<Int> is not ShareSafe, therefore Shared<Cell<Int>> is not Transfer
```

### 3-4. ロック保持中の await をコンパイルエラーにする

**ロックガードは `!Transfer` かつ `!Suspend`。** したがって：

```xenith
let guard = counter.lock();
some_io().await;          // error: ロックガードは中断点を跨げない
```

これは「ロックを持ったまま await する」というLLM頻出バグを**表現不能**にする。
借用チェッカ無しでこれが達成できるのが、この設計の要点。

### 3-5. ゲーム向けゼロコピー

`Region<T>: Transfer` を用意する。`split()` は1つのリージョンを消費し、
**互いに素であることが証明された**複数のリージョンを返す。
素のアリーナビューは `!Transfer` のまま。これでフレーム内並列がコピーなしで書ける。

---

## 4. LLM執筆性（決定基準）

4モデルのランキング（1位＝最も正しく書ける）：

| 順位 | codex | grok | opencode |
|---|---|---|---|
| 1 | 構造化async＋アフィン捕獲 | 構造化async＋Send障壁 | 構造化async＋move-only境界 |
| 2 | 隔離アクタ | 非構造async | アクタ |
| 3 | 明示的タスクグラフ | チャネル＋スレッド | ジョブグラフ |
| 下位 | 非構造async → 緑スレッド → OSスレッド | アクタ → **緑スレッド** → — | 非構造async → **緑スレッド** → OSスレッド |

（agy のみ緑スレッド系を1位に置いた。）

> ⚠️ **証拠の扱いに関する注記。** agy は各方式に pass@1 の具体的数値
> （約88% / 62% / 41% / 35%）を付したが、**出典が無く再現もできない。数値として採用しない。**
> 逆に codex は「これらの言語モデルを直接比較した信頼できる実証研究はまだ存在しない。
> 本ランキングは設計判断である」と明示した。この誠実さの方を採る。
> **本文書のランキングは合議による設計判断であって、測定ではない。**
> 実測は `bench/ai/` の並行性タスク群で行う。

### 表現不能になるLLM頻出バグ

| バグ | 何が防ぐか |
|---|---|
| join 忘れ・孤児タスク | スコープが子の完了なしに脱出できない |
| 複数タスクが可変エイリアスを捕獲 | アフィン捕獲が拒否 |
| ループ変数の捕獲 | 捕獲はその反復の値を**ムーブ**する |
| 子の失敗の握り潰し | スコープが伝播させる |
| ロック保持中の await | ロックガードが `!Suspend` |
| イベントループのブロッキング | 実行器と効果の不一致で拒否 |
| 無制限 spawn | 有界スコープ。`spawn` は `Result<Join<T>, Full>` を返す |

### 依然として表現可能なもの（正直に）

**競合排除は並行性の正しさ一般ではない。**
チャネルのプロトコルデッドロック、メッセージ順序の誤り、飢餓、作業分割の不備は防げない。
これらは lint と文書（「ロックよりチャネルを選べ」）で扱う。

メモリモデルは **DRF-SC**（データ競合が無ければ逐次一貫）を採り、
happens-before は join・チャネル・ロック・アトミクスを通じて定義する。

---

## 5. 実装順序（個人向け）

1. 0003 のカーネル表を仕様に落とす
2. `Transfer` / `ShareSafe` 導出 ＋ `Mutex` / `Cell`
3. `Task` ＋ `Scope` ＋ 単一スレッド実行器
4. 多スレッドのサーバー実行器（`IoExecutor`）
5. ゲーム実行器（`FrameExecutor`：フレーム固定＋ワーカー）
6. `parallel_for` をライブラリとして

**この順序は v0.3 以降に着手する。** v0.1〜v0.2 の間は単一スレッド実行器だけを持ち、
型付きホールとクエリAPI（0002 §1）の完成を優先する。
ただし `Transfer` / `ShareSafe` の**型システム上の場所は最初から空けておく**。
後から入れると型システム全体をやり直すことになるため。
