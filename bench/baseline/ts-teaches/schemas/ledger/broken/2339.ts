import { openLedger, recordEntry, settleBalance } from "./lib";

const rentAmount = 320;
let ledger = openLedger("JPY", 100);
ledger = recordEntry(ledger, "rent", rentAmount);
console.log("currency=" + ledger.money);
console.log("entries=" + ledger.entries.length);
console.log("balance=" + settleBalance(ledger, true));
