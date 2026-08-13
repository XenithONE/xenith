# ts-teaches — design/0019 の基線計器

「teaches（誤り地点への正確なシグネチャの自発添付）の符号は TypeScript に移るか」を
測るための装置。設計と事前登録は [design/0019](../../../design/0019-cross-language-baseline.md)。

## 手順−1: 重複測定（実施済み）

LLM を1走も焼く前に、tsc の素診断が teaches ペイロードの事実をどれだけ既に含むかを
静的に測る。結果は [overlap-report.md](overlap-report.md) — **4族すべて生存**
（素診断に無い新規事実: TS2339 は 4/4、TS2551 は 3/4、TS2345 は 6/8、TS2554 は 7/8）。

```bash
npm run overlap
```

## 固定版の理由

`typescript` は **5.9.3 に厳密固定**。最新の 7.0.2（ネイティブ実装世代)は
`createProgram` / TypeChecker の JS API を露出しておらず（導入して実測）、
計器が要る Compiler API は 5.x 系列が最終。モデルの修復事前分布も 5.x 世代の
診断文で形成されているため、測定対象としても 5.9 が正当。
依存はこの1本のみ・`--ignore-scripts` で導入・lockfile コミット。
