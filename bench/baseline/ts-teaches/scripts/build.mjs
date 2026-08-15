// 変異の適用と凍結の検証。
//
// 既定（--check）: 参照 client から broken/*.ts と expected.txt を再生成し、
// コミット済みバイトと一致することを断言する（凍結後の無断ドリフト防止）。
// --write: 生成物を書き出す（凍結の更新は意図的な操作としてのみ）。
//
// 各変異は「ちょうど1回適用できること」と「期待した診断コードが実際に
// 出ること」を生成時に断言する — 想像した罠ではなく実測の罠だけを凍結する。

import { readFileSync, writeFileSync, mkdirSync, readdirSync, existsSync } from "node:fs";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { compile } from "./teaches.mjs";
import { judge } from "./oracle.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const write = process.argv.includes("--write");
let failed = false;

function problem(msg) {
    failed = true;
    console.error(`NG: ${msg}`);
}

function expectDiagnostic(schema, family, brokenText) {
    const dir = mkdtempSync(join(tmpdir(), "ts-teaches-build-"));
    try {
        writeFileSync(join(dir, "lib.ts"), readFileSync(join(root, "schemas", schema, "lib.ts")));
        writeFileSync(join(dir, "client.ts"), brokenText);
        const { diags } = compile(dir);
        if (!diags.some((d) => d.code === Number(family))) {
            problem(
                `${schema}/${family}: 期待コードが出ない。実際: ${diags.map((d) => "TS" + d.code).join(",") || "なし"}`,
            );
        }
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
}

function emit(path, content, label) {
    if (write) {
        mkdirSync(dirname(path), { recursive: true });
        writeFileSync(path, content);
        console.log(`書出: ${label}`);
    } else if (!existsSync(path)) {
        problem(`${label}: 凍結ファイルが無い（--write で生成する）`);
    } else if (readFileSync(path, "utf8") !== content) {
        problem(`${label}: 再生成がコミット済みバイトと一致しない — 凍結が破れている`);
    }
}

for (const schema of readdirSync(join(root, "schemas"))) {
    const schemaDir = join(root, "schemas", schema);
    const reference = readFileSync(join(schemaDir, "client.ts"), "utf8");
    const meta = (await import(pathToFileURL(join(schemaDir, "meta.mjs")))).default;

    const verdict = judge(schemaDir, reference, null);
    if (!verdict.ok) {
        problem(`${schema}: 参照 client がオラクルを通らない — ${verdict.stage}: ${verdict.detail}`);
        continue;
    }
    emit(join(schemaDir, "expected.txt"), verdict.stdout, `${schema}/expected.txt`);

    for (const [family, m] of Object.entries(meta.mutations)) {
        const first = reference.indexOf(m.find);
        if (first === -1) {
            problem(`${schema}/${family}: 変異対象 '${m.find}' が client に無い`);
            continue;
        }
        if (reference.indexOf(m.find, first + 1) !== -1) {
            problem(`${schema}/${family}: 変異対象 '${m.find}' が複数回出現 — 一意でない`);
            continue;
        }
        const broken = reference.replace(m.find, m.replace);
        expectDiagnostic(schema, family, broken);
        emit(join(schemaDir, "broken", `${family}.ts`), broken, `${schema}/broken/${family}.ts`);
    }
}

if (failed) process.exit(1);
console.log(write ? "生成完了" : "凍結一致");
