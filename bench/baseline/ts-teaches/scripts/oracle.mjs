// 判定オラクル: 候補 client.ts を schema の lib に対して
//   1. 型検査（診断ゼロ）
//   2. CommonJS へ emit して実行
//   3. stdout を参照出力と厳密比較
//   4. lib の関数が実際に呼ばれたことを実行トレースで検証
//      （定数出力・再実装の抜け道封じ — design/0019 §2）
//
// 実行は lib の exports をトレースでラップしてから client を require する。
// CommonJS ではモジュールキャッシュが共有されるため、先に差し替えれば
// client の分割 import も差し替え後の関数を掴む。

import ts from "typescript";
import {
    mkdtempSync,
    writeFileSync,
    readFileSync,
    rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { OPTIONS } from "./teaches.mjs";

const RUNNER = `const path = require("path");
const lib = require(path.join(__dirname, "lib.js"));
const calls = [];
for (const key of Object.keys(lib)) {
    if (typeof lib[key] === "function") {
        const original = lib[key];
        lib[key] = (...args) => {
            calls.push(key);
            return original(...args);
        };
    }
}
process.on("exit", () => {
    process.stderr.write("__CALLS__" + JSON.stringify([...new Set(calls)]));
});
require(path.join(__dirname, "client.js"));
`;

export function judge(schemaDir, clientText, expectedStdout) {
    const dir = mkdtempSync(join(tmpdir(), "ts-teaches-oracle-"));
    try {
        writeFileSync(join(dir, "lib.ts"), readFileSync(join(schemaDir, "lib.ts")));
        writeFileSync(join(dir, "client.ts"), clientText);

        const options = { ...OPTIONS, noEmit: false, outDir: dir };
        const program = ts.createProgram(
            [join(dir, "client.ts"), join(dir, "lib.ts")],
            options,
        );
        const diags = [
            ...program.getSyntacticDiagnostics(),
            ...program.getSemanticDiagnostics(),
        ];
        if (diags.length > 0) {
            return {
                ok: false,
                stage: "types",
                detail: diags
                    .slice(0, 3)
                    .map((d) => `TS${d.code}: ${ts.flattenDiagnosticMessageText(d.messageText, " ")}`)
                    .join(" / "),
            };
        }
        program.emit();

        writeFileSync(join(dir, "runner.js"), RUNNER);
        const run = spawnSync("node", [join(dir, "runner.js")], {
            timeout: 10_000,
            encoding: "utf8",
        });
        if (run.status !== 0) {
            return { ok: false, stage: "run", detail: (run.stderr ?? "").slice(0, 300) };
        }
        const stdout = run.stdout.replace(/\r\n/g, "\n");
        const callsMatch = (run.stderr ?? "").match(/__CALLS__(\[.*\])/);
        const calls = callsMatch ? JSON.parse(callsMatch[1]) : [];
        if (calls.length === 0) {
            return { ok: false, stage: "trace", detail: "lib の関数が一度も呼ばれていない" };
        }
        if (expectedStdout != null && stdout !== expectedStdout) {
            return { ok: false, stage: "stdout", detail: JSON.stringify({ got: stdout, want: expectedStdout }) };
        }
        return { ok: true, stage: "green", calls, stdout };
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
}

// CLI: node scripts/oracle.mjs <schemaDir> <clientFile>
import { pathToFileURL } from "node:url";
const isMain = process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
if (isMain && process.argv[2] && process.argv[3]) {
    const schemaDir = process.argv[2];
    const clientText = readFileSync(process.argv[3], "utf8");
    const expected = readFileSync(join(schemaDir, "expected.txt"), "utf8").replace(/\r\n/g, "\n");
    const verdict = judge(schemaDir, clientText, expected);
    console.log(JSON.stringify(verdict));
    process.exit(verdict.ok ? 0 : 1);
}
