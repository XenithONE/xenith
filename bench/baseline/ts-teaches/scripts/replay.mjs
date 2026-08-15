// design/0019 の本測定ランナー: 対応付き診断リプレイ。
//
// セル = schema × family × model × arm。同一の壊れた client を新規セッションへ
// 複製し、腕によって診断だけを変えて単発修復させる。モデル起動は凍結起動器
// bench/ai/invoke.ps1 に委譲する（中立 cwd・フラグ順の罠は全てそこが持つ）。
//
// 結果は results/replay.jsonl に1セル1行で追記。既に記録のあるセルは飛ばす
// （resume）。空応答・抽出失敗は verdict とは別の status で記録する（agy 罠）。
//
// 使い方:
//   node scripts/replay.mjs --dry                 全セルのプロンプトを検査だけする
//   node scripts/replay.mjs --model codex         1モデルだけ走らせる
//   node scripts/replay.mjs                       全モデル・全セル（resume）

import {
    readFileSync,
    writeFileSync,
    appendFileSync,
    readdirSync,
    existsSync,
    mkdirSync,
    mkdtempSync,
    rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";
import { compile, renderNative, renderTeaching } from "./teaches.mjs";
import { judge } from "./oracle.mjs";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = join(root, "..", "..", "..");
const invoker = join(repoRoot, "bench", "ai", "invoke.ps1");

const MODELS = [
    "codex",
    "grok",
    "agy",
    "opencode",
    "opencode-deepseek",
    "opencode-nemotron",
    "cursor",
];
const FAMILIES = ["2339", "2551", "2345", "2554"];
const ARMS = ["native", "teach"];
const TIMEOUT_MS = 420_000;

const dry = process.argv.includes("--dry");
const modelFilter = process.argv.includes("--model")
    ? process.argv[process.argv.indexOf("--model") + 1]
    : null;
const limit = process.argv.includes("--limit")
    ? Number(process.argv[process.argv.indexOf("--limit") + 1])
    : Infinity;

const template = readFileSync(join(root, "prompts", "repair.md"), "utf8");
const schemas = readdirSync(join(root, "schemas"));

// 並列走行（モデル別プロセス）で追記が混線しないよう、書き込み先は
// モデル別ファイル。resume の既走判定は results/ 全ファイルを読む。
mkdirSync(join(root, "results"), { recursive: true });
const resultsPath = join(root, "results", `replay-${modelFilter ?? "all"}.jsonl`);
const done = new Set();
for (const file of readdirSync(join(root, "results"))) {
    if (!file.endsWith(".jsonl")) continue;
    for (const line of readFileSync(join(root, "results", file), "utf8").split("\n")) {
        if (!line.trim()) continue;
        const r = JSON.parse(line);
        done.add(`${r.schema}/${r.family}/${r.model}/${r.arm}`);
    }
}

// 診断は凍結時に1回だけレンダリングし、全モデル・全腕で同一バイトを使う。
function renderCell(schemaDir, family) {
    const broken = readFileSync(join(schemaDir, "broken", `${family}.ts`), "utf8");
    const dir = mkdtempSync(join(tmpdir(), "ts-teaches-replay-"));
    try {
        writeFileSync(join(dir, "lib.ts"), readFileSync(join(schemaDir, "lib.ts")));
        writeFileSync(join(dir, "client.ts"), broken);
        const { program, sf, diags } = compile(dir);
        if (!diags.some((d) => d.code === Number(family))) {
            throw new Error(`計器の失敗: ${schemaDir}/${family} で期待コードが出ない`);
        }
        const native = renderNative(dir, diags);
        const teach = renderTeaching(dir, program, sf, diags);
        if (!teach.includes("  teach: ")) {
            throw new Error(`計器の失敗: ${schemaDir}/${family} で teach 行が出ない`);
        }
        if (native.includes("  teach: ")) {
            throw new Error(`計器の失敗: native 出力に teach 行が混入`);
        }
        return { broken, native, teach };
    } finally {
        rmSync(dir, { recursive: true, force: true });
    }
}

function extractCode(reply) {
    const fences = [...reply.matchAll(/```(?:ts|typescript)?\s*\n([\s\S]*?)```/g)];
    if (fences.length > 0) return fences[fences.length - 1][1];
    if (reply.includes("import ")) return reply.trim() + "\n";
    return null;
}

let ran = 0;
for (const schema of schemas) {
    const schemaDir = join(root, "schemas", schema);
    const expected = readFileSync(join(schemaDir, "expected.txt"), "utf8");
    for (const family of FAMILIES) {
        const cell = renderCell(schemaDir, family);
        for (const model of MODELS) {
            if (modelFilter && model !== modelFilter) continue;
            for (const arm of ARMS) {
                if (ran >= limit) {
                    console.log(`--limit ${limit} に到達`);
                    process.exit(0);
                }
                const key = `${schema}/${family}/${model}/${arm}`;
                if (done.has(key)) continue;
                const diagnostics = arm === "teach" ? cell.teach : cell.native;
                const prompt = template
                    .replace("{{CLIENT}}", cell.broken.trimEnd())
                    .replace("{{DIAGNOSTICS}}", diagnostics.trimEnd());

                if (dry) {
                    console.log(`dry: ${key} promptBytes=${Buffer.byteLength(prompt)}`);
                    continue;
                }

                const promptFile = join(
                    mkdtempSync(join(tmpdir(), "ts-teaches-prompt-")),
                    "prompt.md",
                );
                writeFileSync(promptFile, prompt);
                const started = Date.now();
                const proc = spawnSync(
                    "pwsh",
                    ["-NoProfile", "-File", invoker, "-Cli", model, "-PromptFile", promptFile],
                    { timeout: TIMEOUT_MS, encoding: "utf8" },
                );
                rmSync(dirname(promptFile), { recursive: true, force: true });

                const reply = (proc.stdout ?? "").trim();
                const record = {
                    ts: new Date().toISOString(),
                    schema,
                    family,
                    model,
                    arm,
                    promptBytes: Buffer.byteLength(prompt),
                    teachBytes:
                        arm === "teach"
                            ? Buffer.byteLength(cell.teach) - Buffer.byteLength(cell.native)
                            : 0,
                    seconds: Math.round((Date.now() - started) / 1000),
                };
                if (proc.error?.code === "ETIMEDOUT") {
                    record.status = "timeout";
                } else if (reply.length === 0) {
                    record.status = "empty";
                } else {
                    const code = extractCode(reply);
                    if (code == null) {
                        record.status = "no-code";
                        record.replyHead = reply.slice(0, 200);
                    } else {
                        record.status = "answered";
                        const verdict = judge(schemaDir, code, expected);
                        record.verdict = verdict;
                        record.green = verdict.ok;
                    }
                }
                appendFileSync(resultsPath, JSON.stringify(record) + "\n");
                ran += 1;
                console.log(
                    `${key}: ${record.status}${record.green != null ? ` green=${record.green}` : ""} (${record.seconds ?? "-"}s)`,
                );
            }
        }
    }
}
console.log(dry ? "dry 完了" : `${ran} セル実行`);
