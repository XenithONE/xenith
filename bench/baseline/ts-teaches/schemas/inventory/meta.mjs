// includeEmpty は参照データに count=0 の品目が無いので true/false どちらでも
// stock は同値 — 2554 の修復が挿す値に stdout が依存しない。
export default {
    mutations: {
        2339: { find: "store.name", replace: "store.storeName" },
        2551: { find: "store.name", replace: "store.namee" },
        2345: { find: 'addItem(store, "bolt", boltCount)', replace: 'addItem(store, boltCount, "bolt")' },
        2554: { find: "countStock(store, true)", replace: "countStock(store)" },
    },
};
