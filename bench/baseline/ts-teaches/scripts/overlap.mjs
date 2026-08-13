// design/0019 手順−1: LLM を1走も焼く前に、tsc の素診断が teaches ペイロードの
// 事実をどれだけ既に含んでいるかを測る。
//
// 各バグ族について:
//   1. 凍結ドメイン(lib.ts)に対するバグ注入 client を tsc にかけ、素診断文を得る
//   2. teaches が添付するはずの事実(完全シグネチャ・メンバー表)を TypeChecker から導出
//   3. 事実を原子単位(引数名・引数型・戻り型・個数・メンバー名・メンバー型)に割り、
//      素診断文への出現を機械判定する
//
// 事前登録の生存規則: 新規事実(素診断に無い事実)が 0 件の族は治療から外す。
// 期待診断コードが出なかった場合はこの計器自体の失敗として非零終了する。

import ts from "typescript";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const LIB = `export interface Entry {
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
`;

// バグ族: 名前 → { client ソース, 期待診断コード, teaches の対象 }
const FAMILIES = {
    "unknown-member (TS2339)": {
        client: `import { openLedger } from "./lib";
const ledger = openLedger("JPY", 100);
console.log(ledger.total);
`,
        expect: 2339,
        teaches: { kind: "members", type: "Ledger" },
    },
    "name-near-miss (TS2551)": {
        client: `import { openLedger } from "./lib";
const ledger = openLedger("JPY", 100);
console.log(ledger.curency);
`,
        expect: 2551,
        teaches: { kind: "members", type: "Ledger" },
    },
    "argument-type (TS2345)": {
        client: `import { openLedger, recordEntry } from "./lib";
const ledger = openLedger("JPY", 100);
recordEntry(ledger, 42, "rent");
`,
        expect: 2345,
        teaches: { kind: "signature", fn: "recordEntry" },
    },
    "argument-count (TS2554)": {
        client: `import { openLedger, recordEntry } from "./lib";
const ledger = openLedger("JPY", 100);
recordEntry(ledger, "rent");
`,
        expect: 2554,
        teaches: { kind: "signature", fn: "recordEntry" },
    },
};

const OPTIONS = {
    strict: true,
    target: ts.ScriptTarget.ES2020,
    module: ts.ModuleKind.ES2020,
    noEmit: true,
};

function diagnose(dir, clientSource) {
    const clientPath = join(dir, "client.ts");
    writeFileSync(clientPath, clientSource);
    const program = ts.createProgram([clientPath], OPTIONS);
    const sf = program.getSourceFile(clientPath);
    const diags = program.getSemanticDiagnostics(sf).map((d) => ({
        code: d.code,
        text: ts.flattenDiagnosticMessageText(d.messageText, " "),
    }));
    return { program, diags };
}

// teaches が添付する事実を原子単位に割る。fact = { label, needle }
// needle が素診断文に部分一致すれば「素診断が既に言っている」。
function teachesFacts(program, spec) {
    const checker = program.getTypeChecker();
    const lib = program
        .getSourceFiles()
        .find((f) => f.fileName.replace(/\\/g, "/").endsWith("/lib.ts"));
    const facts = [];
    if (spec.kind === "members") {
        lib.forEachChild((node) => {
            if (ts.isInterfaceDeclaration(node) && node.name.text === spec.type) {
                const type = checker.getTypeAtLocation(node);
                for (const prop of type.getProperties()) {
                    const propType = checker.typeToString(
                        checker.getTypeOfSymbolAtLocation(prop, node),
                    );
                    facts.push({ label: `member name '${prop.name}'`, needle: prop.name });
                    facts.push({
                        label: `member type '${prop.name}: ${propType}'`,
                        needle: `${prop.name}: ${propType}`,
                    });
                }
            }
        });
    } else {
        lib.forEachChild((node) => {
            if (ts.isFunctionDeclaration(node) && node.name?.text === spec.fn) {
                const sig = checker.getSignatureFromDeclaration(node);
                const params = sig.getParameters();
                facts.push({
                    label: `arity '${params.length} arguments'`,
                    needle: `${params.length} argument`,
                });
                for (const p of params) {
                    const pType = checker.typeToString(
                        checker.getTypeOfSymbolAtLocation(p, node),
                    );
                    facts.push({ label: `param name '${p.name}'`, needle: p.name });
                    facts.push({ label: `param type '${pType}'`, needle: pType });
                }
                facts.push({
                    label: `return type '${checker.typeToString(sig.getReturnType())}'`,
                    needle: checker.typeToString(sig.getReturnType()),
                });
            }
        });
    }
    return facts;
}

const dir = mkdtempSync(join(tmpdir(), "ts-teaches-overlap-"));
writeFileSync(join(dir, "lib.ts"), LIB);

let failed = false;
console.log("# 0019 手順−1 — tsc 素診断と teaches ペイロードの重複測定");
console.log();
console.log(`typescript ${ts.version} / 生存規則: 新規事実 0 件の族を治療から外す`);
console.log();

for (const [family, spec] of Object.entries(FAMILIES)) {
    const { program, diags } = diagnose(dir, spec.client);
    const hit = diags.find((d) => d.code === spec.expect);
    console.log(`## ${family}`);
    console.log();
    if (!hit) {
        failed = true;
        console.log(
            `**計器の失敗**: 期待コード TS${spec.expect} が出ない。実際: ${
                diags.map((d) => `TS${d.code}: ${d.text}`).join(" / ") || "(診断なし)"
            }`,
        );
        console.log();
        continue;
    }
    console.log(`素診断: \`TS${hit.code}: ${hit.text}\``);
    console.log();
    const facts = teachesFacts(program, spec.teaches);
    let fresh = 0;
    for (const fact of facts) {
        const inNative = hit.text.includes(fact.needle);
        if (!inNative) fresh += 1;
        console.log(`- ${inNative ? "既出" : "**新規**"} — ${fact.label}`);
    }
    console.log();
    console.log(
        `事実 ${facts.length} 件中、素診断に既出 ${facts.length - fresh} 件・新規 ${fresh} 件 → **${
            fresh > 0 ? "生存" : "脱落"
        }**`,
    );
    console.log();
}

rmSync(dir, { recursive: true, force: true });
if (failed) process.exit(1);
