export interface Item {
    sku: string;
    count: number;
}

export interface Store {
    items: Item[];
    name: string;
}

export function createStore(name: string, capacity: number): Store {
    return { items: [{ sku: "pallet", count: capacity > 0 ? 1 : 0 }], name };
}

export function addItem(store: Store, sku: string, count: number): Store {
    return { ...store, items: [...store.items, { sku, count }] };
}

export function countStock(store: Store, includeEmpty: boolean): number {
    const items = includeEmpty ? store.items : store.items.filter((i) => i.count > 0);
    return items.reduce((sum, i) => sum + i.count, 0);
}
