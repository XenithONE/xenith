export interface Entry {
    label: string;
    amount: number;
}

export interface Ledger {
    entries: Entry[];
    currency: string;
}

export function openLedger(currency: string, initial: number): Ledger {
    return { entries: [{ label: "initial", amount: initial }], currency };
}

export function recordEntry(ledger: Ledger, label: string, amount: number): Ledger {
    return { ...ledger, entries: [...ledger.entries, { label, amount }] };
}

export function settleBalance(ledger: Ledger, round: boolean): number {
    const total = ledger.entries.reduce((sum, e) => sum + e.amount, 0);
    return round ? Math.round(total) : total;
}
