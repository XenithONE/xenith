# 0013 — 真実源の統一: MCP プロジェクトモードと ApiSurface

- 日付: 2026-08-08
- 状態: **採択（0012 の5AIレビュー裁定の具体化 — 新規争点なし・再レビュー省略）**
- 根拠: 0011 監査の発見「MCP は常に単一ファイルモード」（無言の二重真実源）と、
  0012 レビュー Q2/Q3 の設計制約。実測: api-dump は実装課題の最強文書（17/56）だが
  配線には無力（0/28）— ツール説明にこの限定を書く義務を含む

## 1. ProjectSnapshot — 単一実装（0012 Q2 裁定）

- CLI と MCP が共有する `ProjectRequest → ProjectSnapshot → 解析` を xenith-driver に
  一本化。discovery・封じ込め・モード選択のロジックは**一箇所**にだけ存在する
- MCP ツール（check/run/goals/query）は `mode: auto | project | single_file` を受ける。
  既定 auto = マニフェスト発見で project、無ければ single_file。
  **発見の失敗（壊れたマニフェスト・封じ込め違反・不正レイアウト）は明示エラー** —
  単一ファイルへの無言フォールバック禁止（相対パス退化バグと同類の根絶）
- 封じ込め: 入口ファイルだけでなく**プロジェクトが推移的に読む全ファイル**を正規化後に
  workspace-root 検査
- 応答は実際の `analysis_mode` と project root（root 相対）を返す — features の
  `project_mode_v1` は能力広告であり証明ではない、の区別（codex）
- 診断の順序: 要求ファイルの診断を先頭に、他ファイルはパス辞書順で後置
  （単一ファイル要求へのカスケード殺到の緩和 — agy の裁定を「優先順位付け」で具体化。
  切り捨てはしない — エージェントには他ファイルの失敗も必要）
- 同値検査: CLI と MCP は**正規化診断**（code / severity / root相対 span / message /
  teaches）で一致。レンダリングのバイト golden は各表面が別に持つ

## 2. ApiSurface — 意味モデルと3レンダラ（0012 Q3 裁定）

- 共有するのはレンダラではなく **ApiSurface 意味モデル**（到達可能な公開 API の構造:
  モジュール → pub fn シグネチャ / pub struct / pub enum / 効果集合、決定的順序）
- レンダラ3枚: (a) text（人間/エージェント向け）(b) JSON（**payload 自身に独立
  `api_schema_version: 1` を内蔵** — wire の schema_version とは別系・破壊時は番号を
  上げる）(c) ベンチ dump（既存形式）
- **characterization gate**: 凍結済み bench/ai/tasks-t5/*/api-dump.txt が新モデル経由の
  再生成とバイト一致すること。凍結形式が意味モデルを拘束しないよう、ベンチレンダラは
  互換層として隔離
- CLI: `xenith api <project> [--module <path>] [--json]`。モジュール単位の絞り込みが
  一級（全量 dump はトークン限界を割る — agy）
- MCP: `api_surface` ツールは **experimental 隔離** — 既定のツール一覧に載せず、
  サーバ起動フラグ `--experimental-api-surface` でのみ露出。ツール説明に
  「モジュール配線の代替ではない（0011: 配線 0/28）」を明記（grok）
- エージェントの自発的ツール発見・利用の効果測定は本 RFC の範囲外（別測定 RFC）

## 3. 実装順・検証

1. ProjectSnapshot 抽出（CLI 移行・挙動不変 — 既存 CLI テスト全緑が回帰ゲート）
2. MCP 統合＋mode 契約＋明示エラー＋正規化診断同値テスト（3系: マニフェスト内
   複数ファイル / マニフェスト外単一 / root 外拒否）
3. ApiSurface モデル＋characterization gate＋CLI＋experimental MCP
4. verify 22 参照解・凍結フィクスチャ・0011/0012 結果はすべて不変

## 4. 入れないもの

- 未保存バッファ / overlay（MCP クライアントのファイル同期は将来 RFC — codex の
  指摘は認識済みだが、現行 MCP は常にディスク読みでありその前提を維持）
- JSON スキーマの安定宣言（experimental のまま。安定化条件は別 RFC）
- パッケージ間依存・std 物理分割（従来どおり範囲外）
