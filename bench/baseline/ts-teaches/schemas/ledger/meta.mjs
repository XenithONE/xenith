// 変異は宣言的に1箇所ずつ。build.mjs が「ちょうど1回適用できたこと」と
// 「期待した診断コードが実際に出ること」の両方を断言する。
// stdout 不変則: 2554 で落とす boolean は、参照データ上どちらを渡しても
// 同じ出力になる位置しか選ばない（整数合計に round は無効果)。
export default {
    mutations: {
        2339: { find: "ledger.currency", replace: "ledger.money" },
        2551: { find: "ledger.currency", replace: "ledger.curency" },
        2345: { find: 'recordEntry(ledger, "rent", rentAmount)', replace: 'recordEntry(ledger, rentAmount, "rent")' },
        2554: { find: "settleBalance(ledger, true)", replace: "settleBalance(ledger)" },
    },
};
