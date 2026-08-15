// design/0019 の集計。results/*.jsonl を読み、事前登録の判定規則を適用する。
//
// 主判定（事前登録 §3）: 同一 client・同一モデルのペアで、両腕が回答した
// もののうち不一致ペア（片方だけ緑）の符号検定（両側）。
// 併記: ITT（非回答=失敗扱い）の率差、非回答の別掲、腕別の非回答対称性。

import { readFileSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const rows = [];
for (const f of readdirSync(join(root, "results"))) {
    if (!f.endsWith(".jsonl")) continue;
    for (const line of readFileSync(join(root, "results", f), "utf8").split("\n")) {
        if (line.trim()) rows.push(JSON.parse(line));
    }
}

const pairs = new Map();
for (const r of rows) {
    const key = `${r.model}|${r.schema}|${r.family}`;
    if (!pairs.has(key)) pairs.set(key, {});
    pairs.get(key)[r.arm] = r;
}

const perModel = new Map();
function bucket(model) {
    if (!perModel.has(model)) {
        perModel.set(model, {
            teachWin: 0, nativeWin: 0, bothGreen: 0, bothFail: 0,
            incomplete: 0, ittNativeGreen: 0, ittTeachGreen: 0, cells: 0,
        });
    }
    return perModel.get(model);
}

let teachWin = 0, nativeWin = 0, bothGreen = 0, bothFail = 0, incomplete = 0;
const teachWinDetail = [];
for (const [key, p] of [...pairs.entries()].sort()) {
    const model = key.split("|")[0];
    const b = bucket(model);
    b.cells += 1;
    const nG = p.native?.status === "answered" && p.native.green;
    const tG = p.teach?.status === "answered" && p.teach.green;
    if (nG) b.ittNativeGreen += 1;
    if (tG) b.ittTeachGreen += 1;
    if (p.native?.status !== "answered" || p.teach?.status !== "answered") {
        incomplete += 1; b.incomplete += 1; continue;
    }
    if (tG && !nG) { teachWin += 1; b.teachWin += 1; teachWinDetail.push(key); }
    else if (nG && !tG) { nativeWin += 1; b.nativeWin += 1; }
    else if (nG && tG) { bothGreen += 1; b.bothGreen += 1; }
    else { bothFail += 1; b.bothFail += 1; }
}

console.log("| model | 両緑 | teach勝 | native勝 | 両失敗 | 不完全 | ITT native | ITT teach |");
console.log("| --- | --- | --- | --- | --- | --- | --- | --- |");
for (const [model, b] of [...perModel.entries()].sort()) {
    console.log(
        `| ${model} | ${b.bothGreen} | ${b.teachWin} | ${b.nativeWin} | ${b.bothFail} | ${b.incomplete} | ${b.ittNativeGreen}/${b.cells} | ${b.ittTeachGreen}/${b.cells} |`,
    );
}
console.log();
const n = teachWin + nativeWin;
// 両側符号検定の厳密 p 値
function signTestP(n, w) {
    const comb = (n, k) => {
        let r = 1;
        for (let i = 0; i < k; i++) r = (r * (n - i)) / (i + 1);
        return r;
    };
    const extreme = Math.max(w, n - w);
    let tail = 0;
    for (let k = extreme; k <= n; k++) tail += comb(n, k);
    return Math.min(1, 2 * tail * Math.pow(0.5, n));
}
console.log(`不一致ペア: ${n}（teach勝 ${teachWin} / native勝 ${nativeWin}）`);
console.log(`両緑 ${bothGreen} / 両失敗 ${bothFail} / 不完全ペア（別掲） ${incomplete}`);
if (n > 0) {
    console.log(`符号検定（両側・厳密）: p = ${signTestP(n, teachWin).toExponential(3)}`);
    console.log(`ペア勝率（teach）: ${(teachWin / n * 100).toFixed(1)}%`);
}
console.log();
console.log("teach 勝ちの内訳（family 別）:");
const byFam = {};
for (const k of teachWinDetail) byFam[k.split("|")[2]] = (byFam[k.split("|")[2]] ?? 0) + 1;
console.log(JSON.stringify(byFam));
