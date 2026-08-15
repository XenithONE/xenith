import { createStore, addItem, countStock } from "./lib";

const boltCount = 25;
let store = createStore("main", 100);
store = addItem(store, "bolt", boltCount);
console.log("name=" + store.storeName);
console.log("items=" + store.items.length);
console.log("stock=" + countStock(store, true));
