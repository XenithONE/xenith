# 0019 手順−1 — tsc 素診断と teaches ペイロードの重複測定

typescript 5.9.3 / 生存規則: 新規事実 0 件の族を治療から外す

## unknown-member (TS2339)

素診断: `TS2339: Property 'total' does not exist on type 'Ledger'.`

- **新規** — member name 'entries'
- **新規** — member type 'entries: Entry[]'
- **新規** — member name 'currency'
- **新規** — member type 'currency: string'

事実 4 件中、素診断に既出 0 件・新規 4 件 → **生存**

## name-near-miss (TS2551)

素診断: `TS2551: Property 'curency' does not exist on type 'Ledger'. Did you mean 'currency'?`

- **新規** — member name 'entries'
- **新規** — member type 'entries: Entry[]'
- 既出 — member name 'currency'
- **新規** — member type 'currency: string'

事実 4 件中、素診断に既出 1 件・新規 3 件 → **生存**

## argument-type (TS2345)

素診断: `TS2345: Argument of type 'number' is not assignable to parameter of type 'string'.`

- **新規** — arity '3 arguments'
- **新規** — param name 'ledger'
- **新規** — param type 'Ledger'
- **新規** — param name 'label'
- 既出 — param type 'string'
- **新規** — param name 'amount'
- 既出 — param type 'number'
- **新規** — return type 'Ledger'

事実 8 件中、素診断に既出 2 件・新規 6 件 → **生存**

## argument-count (TS2554)

素診断: `TS2554: Expected 3 arguments, but got 2.`

- 既出 — arity '3 arguments'
- **新規** — param name 'ledger'
- **新規** — param type 'Ledger'
- **新規** — param name 'label'
- **新規** — param type 'string'
- **新規** — param name 'amount'
- **新規** — param type 'number'
- **新規** — return type 'Ledger'

事実 8 件中、素診断に既出 1 件・新規 7 件 → **生存**

