import { createStore, addItem, countStock } from "./lib";

const boltCount = 25;
let store = createStore("main", 100);
store = addItem(store, boltCount, "bolt");
console.log("name=" + store.name);
console.log("items=" + store.items.length);
console.log("stock=" + countStock(store, true));
