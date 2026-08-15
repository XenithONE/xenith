// ts-teaches の中核: tsc 診断の素レンダリングと、teach 行の付加。
//
// 対象診断（0019 手順−1 の生存4族）:
//   TS2339 / TS2551 / TS2552 — メンバー系: 対象型のメンバー表を教える
//   TS2345 / TS2554        — 呼出し系: 呼び先の完全シグネチャを教える
//
// 規律（0009 準拠）:
//   - `--teaching=off` の出力は素の tsc レンダリングとバイト同一
//     （on の出力から teach 行を取り除くと off と一致 — selftest が断言）
//   - teach は重複排除し、1回の出力で TEACH_BUDGET 本まで
//   - teach は事実のみ（シグネチャ・メンバー表）。指示や修復案は書かない

import ts from "typescript";
import { join } from "node:path";

export const MEMBER_CODES = new Set([2339, 2551, 2552]);
export const CALL_CODES = new Set([2345, 2554]);
export const TEACH_BUDGET = 4;

export const OPTIONS = {
    strict: true,
    target: ts.ScriptTarget.ES2020,
    module: ts.ModuleKind.CommonJS,
    noEmit: true,
};

export function compile(dir) {
    const clientPath = join(dir, "client.ts");
    const program = ts.createProgram([clientPath], { ...OPTIONS });
    const sf = program.getSourceFile(clientPath);
    const diags = [
        ...program.getSyntacticDiagnostics(sf),
        ...program.getSemanticDiagnostics(sf),
    ];
    return { program, sf, diags };
}

const formatHost = {
    getCanonicalFileName: (f) => f,
    getNewLine: () => "\n",
};

// 素の tsc と同じ形: `client.ts(3,20): error TS2339: ...`
// パスはディレクトリ前置きを剥がして安定化する。
export function renderNative(dir, diags) {
    const host = {
        ...formatHost,
        getCurrentDirectory: () => dir,
    };
    return ts.formatDiagnostics(diags, host);
}

function nodeAt(sf, pos) {
    function visit(node) {
        if (pos < node.getStart(sf) || pos >= node.getEnd()) return undefined;
        return ts.forEachChild(node, visit) ?? node;
    }
    return visit(sf);
}

function memberTeach(checker, sf, diag) {
    let node = nodeAt(sf, diag.start);
    while (node && !ts.isPropertyAccessExpression(node)) node = node.parent;
    if (!node) return undefined;
    const type = checker.getTypeAtLocation(node.expression);
    const props = type.getProperties();
    if (props.length === 0) return undefined;
    const members = props
        .slice(0, 8)
        .map((p) => {
            const t = checker.typeToString(
                checker.getTypeOfSymbolAtLocation(p, node.expression),
            );
            return `${p.name}: ${t}`;
        })
        .join("; ");
    return `  teach: '${checker.typeToString(type)}' members — ${members}`;
}

function callTeach(checker, sf, diag) {
    let node = nodeAt(sf, diag.start);
    while (node && !ts.isCallExpression(node)) node = node.parent;
    if (!node) return undefined;
    const calleeType = checker.getTypeAtLocation(node.expression);
    const sigs = calleeType.getCallSignatures();
    if (sigs.length === 0) return undefined;
    const sig = sigs[0];
    const name = node.expression.getText(sf);
    const params = sig
        .getParameters()
        .map((p) => {
            const decl = p.valueDeclaration ?? node;
            const t = checker.typeToString(checker.getTypeOfSymbolAtLocation(p, decl));
            return `${p.name}: ${t}`;
        })
        .join(", ");
    return `  teach: ${name}(${params}): ${checker.typeToString(sig.getReturnType())}`;
}

// teaching=on のレンダリング: 素の各診断行の直後に teach 行を差し込む。
// 差し込み以外のバイトは renderNative と同一（selftest が断言する）。
export function renderTeaching(dir, program, sf, diags) {
    const checker = program.getTypeChecker();
    const seen = new Set();
    let budget = TEACH_BUDGET;
    const pieces = [];
    for (const diag of diags) {
        pieces.push(renderNative(dir, [diag]));
        if (budget <= 0 || diag.start === undefined) continue;
        let teach;
        if (MEMBER_CODES.has(diag.code)) teach = memberTeach(checker, sf, diag);
        else if (CALL_CODES.has(diag.code)) teach = callTeach(checker, sf, diag);
        if (teach && !seen.has(teach)) {
            seen.add(teach);
            budget -= 1;
            pieces.push(teach + "\n");
        }
    }
    return pieces.join("");
}

export function isTeachLine(line) {
    return line.startsWith("  teach: ");
}

// CLI: node scripts/teaches.mjs <dir> [--teaching=off]
import { pathToFileURL } from "node:url";
const isMain = process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
if (isMain && process.argv[2]) {
    const dir = process.argv[2];
    const off = process.argv.includes("--teaching=off");
    const { program, sf, diags } = compile(dir);
    const out = off
        ? renderNative(dir, diags)
        : renderTeaching(dir, program, sf, diags);
    process.stdout.write(out);
    process.exit(diags.length === 0 ? 0 : 1);
}
