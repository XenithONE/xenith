// 計器の反証一式。PASS を信用する前に、計器が嘘をつけないことを断言する。
//
//   1. 参照 client は各 schema でオラクル緑（expected.txt と厳密一致）
//   2. 2554 の boolean 反転でも緑 — 「修復が正当に残す自由度に stdout が
//      依存しない」という課題設計の不変則そのものをテストする
//   3. 全 broken client はオラクルで落ちる（types 段）
//   4. teaching=on から teach 行を除くと teaching=off とバイト同一（0009 規律）
//   5. teach 行は各 broken client で必ず1本以上（配達の前提条件）で、
//      修復に必要な事実（正しいメンバー名／全引数名）を実際に含む
//   6. 2339 の素診断は正しいメンバー名を含まない（含むなら対照群が
//      治療済み = 手順−1 の生存判定が破れている）

import { readFileSync, readdirSync, mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { compile, renderNative, renderTeaching, isTeachLine } from "./teaches.mjs";
import { judge } from "./oracle.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
let checks = 0;
let failed = 0;

function assert(cond, label) {
    checks += 1;
    if (!cond) {
        failed += 1;
        console.error(`NG: ${label}`);
    }
}

// 修復に必要な事実: schema ごとの正しいメンバー名と、呼出し系の対象関数
const REPAIR_FACTS = {
    ledger: { member: "currency", callParams: { 2345: ["ledger", "label", "amount"], 2554: ["ledger", "round"] } },
    inventory: { member: "name", callParams: { 2345: ["store", "sku", "count"], 2554: ["store", "includeEmpty"] } },
    textstat: { member: "title", callParams: { 2345: ["doc", "minLength"], 2554: ["report", "distinct"] } },
    routing: { member: "origin", callParams: { 2345: ["route", "label", "km"], 2554: ["route", "roundUp"] } },
};

function inTemp(schemaDir, clientText, fn) {
    const dir = mkdtempSync(join(tmpdir(), "ts-teaches-selftest-"));
    try {
        writeFileSync(join(dir, "lib.ts"), readFileSync(join(schemaDir, "lib.ts")));
        writeFileSync(join(dir, "client.ts"), clientText);
        return fn(dir);
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
}

for (const schema of readdirSync(join(root, "schemas"))) {
    const schemaDir = join(root, "schemas", schema);
    const reference = readFileSync(join(schemaDir, "client.ts"), "utf8");
    const expected = readFileSync(join(schemaDir, "expected.txt"), "utf8");
    const meta = (await import(pathToFileURL(join(schemaDir, "meta.mjs")))).default;
    const facts = REPAIR_FACTS[schema];

    // 1. 参照は緑
    const green = judge(schemaDir, reference, expected);
    assert(green.ok, `${schema}: 参照 client が緑でない — ${green.stage}: ${green.detail ?? ""}`);

    // 2. boolean 反転不変則
    const site = meta.mutations[2554].find;
    const flipped = site.includes("true")
        ? site.replace("true", "false")
        : site.replace("false", "true");
    const flippedClient = reference.replace(site, flipped);
    assert(flippedClient !== reference, `${schema}: 2554 サイトの boolean が見つからない`);
    const flipVerdict = judge(schemaDir, flippedClient, expected);
    assert(
        flipVerdict.ok,
        `${schema}: boolean 反転で stdout が変わる — 2554 の修復自由度が採点に漏れている`,
    );

    for (const family of Object.keys(meta.mutations)) {
        const broken = readFileSync(join(schemaDir, "broken", `${family}.ts`), "utf8");

        // 3. broken は落ちる
        const bad = judge(schemaDir, broken, expected);
        assert(!bad.ok && bad.stage === "types", `${schema}/${family}: broken が types 段で落ちない`);

        inTemp(schemaDir, broken, (dir) => {
            const { program, sf, diags } = compile(dir);
            assert(
                diags.some((d) => d.code === Number(family)),
                `${schema}/${family}: 期待コードが出ない`,
            );

            // 4. バイト同一規律
            const off = renderNative(dir, diags);
            const on = renderTeaching(dir, program, sf, diags);
            const stripped = on
                .split("\n")
                .filter((l) => !isTeachLine(l))
                .join("\n");
            assert(stripped === off, `${schema}/${family}: on から teach 行を除いても off と一致しない`);

            // 5. teach の配達と内容
            const teachLines = on.split("\n").filter(isTeachLine);
            assert(teachLines.length >= 1, `${schema}/${family}: teach 行が出ない`);
            const teachText = teachLines.join("\n");
            if (family === "2339" || family === "2551") {
                assert(
                    teachText.includes(facts.member),
                    `${schema}/${family}: teach が正しいメンバー名 '${facts.member}' を含まない`,
                );
            } else {
                for (const param of facts.callParams[family]) {
                    assert(
                        teachText.includes(param),
                        `${schema}/${family}: teach が引数名 '${param}' を含まない`,
                    );
                }
            }

            // 6. 2339 の素診断は答えを含まない
            if (family === "2339") {
                assert(
                    !off.includes(facts.member),
                    `${schema}/2339: 素診断が正しいメンバー名を含む — 対照群が治療済み`,
                );
            }
        });
    }
}

console.log(`${checks} 検査中 ${failed} 失敗`);
process.exit(failed === 0 ? 0 : 1);
