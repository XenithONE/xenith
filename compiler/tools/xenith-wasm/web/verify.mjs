// Differential check: the wasm module must agree with the native compiler.
//
// This is the gate that makes Path A worth anything (design/0018). The claim
// is "one semantics, two hosts", and the only way to hold a claim like that is
// to run the same programs through both and compare bytes — the same shape as
// the executor-equivalence test the interpreter already carries
// (design/0017 §5).
//
//   node verify.mjs [xenith_wasm.wasm] [xenith(.exe)]
//
// Defaults assume `cargo build -p xenith-wasm --release --target
// wasm32-unknown-unknown --target-dir target-b3` and `cargo build -p xenith
// --target-dir target-b3` have both run.

import { readFileSync, writeFileSync, mkdtempSync, rmSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const targets = resolve(here, "../../../target-b3");
const wasmPath = process.argv[2] ??
  join(targets, "wasm32-unknown-unknown/release/xenith_wasm.wasm");
const nativePath = process.argv[3] ??
  join(targets, process.platform === "win32" ? "debug/xenith.exe" : "debug/xenith");

// ------------------------------------------------------------- the binding

const module_ = await WebAssembly.instantiate(readFileSync(wasmPath), {});
const { memory, xn_alloc, xn_run, xn_free } = module_.instance.exports;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

function runWasm(text) {
  const bytes = encoder.encode(text);
  const input = xn_alloc(bytes.length);
  new Uint8Array(memory.buffer, input, bytes.length).set(bytes);
  const result = xn_run(input, bytes.length);
  const length = new DataView(memory.buffer).getUint32(result, true);
  const json = decoder.decode(new Uint8Array(memory.buffer, result + 4, length));
  xn_free(result, 4 + length);
  return JSON.parse(json);
}

// -------------------------------------------------------------- the native

const scratch = mkdtempSync(join(tmpdir(), "xenith-wasm-"));

function runNative(text) {
  const file = join(scratch, "playground.xn");
  writeFileSync(file, text);
  const done = spawnSync(nativePath, ["run", file], { encoding: "utf8" });
  if (done.error) throw done.error;
  return { exit: done.status, stdout: done.stdout, stderr: done.stderr };
}

// -------------------------------------------------------------- the corpus

const programs = {
  "hello": `fn greeting(name: String) -> String {
    "Hello, ".concat(other: name)
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: greeting(name: "world"))?;
    return Ok(unit);
}
`,
  "arithmetic and loops": `fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    var total = 0;
    var i = 1;
    while i <= 10 {
        total = total + i * i;
        i = i + 1;
    }
    io.write(text: total.to_text())?;
    return Ok(unit);
}
`,
  "tasks": `fn plan(n: Int) -> Int {
    n * 2
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {
    scope {
        let a = spawn plan(n: 1);
        let b = spawn plan(n: 2);
        let total = a.await + b.await;
        io.write(text: total.to_text())?;
    }
    return Ok(unit);
}
`,
  "a trap": `fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: "before")?;
    let n = 1 / 0;
    io.write(text: n.to_text())?;
    return Ok(unit);
}
`,
  "main returns Err": `enum ScoreError {
    Overflow,
}

fn try_double(n: Int) -> Result<Int, ScoreError> {
    n.checked_add(other: n).to_result(error: ScoreError.Overflow)
}

fn main() -> Result<Int, ScoreError> {
    let doubled = try_double(n: 9_223_372_036_854_775_807)?;
    return Ok(doubled);
}
`,
  "an undeclared effect": `fn main(io: Io) -> Result<Unit, Error> {
    io.write(text: "hi")?;
    return Ok(unit);
}
`,
  "an unknown name": `fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: mystery())?;
    return Ok(unit);
}
`,
  "a hole": `fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: ??greeting)?;
    return Ok(unit);
}
`,
};

// --------------------------------------------------------------- the check

let failures = 0;
const width = Math.max(...Object.keys(programs).map((n) => n.length));

for (const [name, text] of Object.entries(programs)) {
  const wasm = runWasm(text);
  const native = runNative(text);
  const problems = [];

  if (wasm.exit !== native.exit) {
    problems.push(`exit ${wasm.exit} (wasm) vs ${native.exit} (native)`);
  }

  if (wasm.exit === 2) {
    // A refused file: the module returns diagnostics structurally, the CLI
    // renders them. Compare what both actually say — code, position, message —
    // rather than the rendering, which is a CLI concern (design/0013 §1 draws
    // the same line between the wire model and its renderers).
    const diagnostics = wasm.report?.diagnostics ?? [];
    if (diagnostics.length === 0) problems.push("exit 2 with no diagnostics");
    for (const d of diagnostics) {
      const rendered = `:${d.line}:${d.column}: ${d.severity}[${d.code}]: ${d.message}`;
      if (!native.stdout.includes(rendered)) {
        problems.push(`${JSON.stringify(rendered)} absent from the native rendering`);
      }
    }
  } else {
    // A file that ran: stdout is the program's own output, byte for byte.
    if (wasm.stdout !== native.stdout) {
      problems.push(`stdout ${JSON.stringify(wasm.stdout)} vs ${JSON.stringify(native.stdout)}`);
    }
    // The trap line the CLI prints on stderr, rebuilt from the wasm result:
    // the path differs by construction, everything after it must not.
    if (wasm.error) {
      const tail = `:${wasm.error.line}:${wasm.error.column}: runtime error: ${wasm.error.message}\n`;
      if (!native.stderr.endsWith(tail)) {
        problems.push(`trap ${JSON.stringify(tail)} not the tail of ${JSON.stringify(native.stderr)}`);
      }
    }
  }

  const note = wasm.exit === 2
    ? (wasm.report?.diagnostics ?? []).map((d) => d.code).join(" ")
    : `stdout ${JSON.stringify(wasm.stdout)}`;
  if (problems.length === 0) {
    console.log(`  ok    ${name.padEnd(width)}  exit ${String(wasm.exit).padStart(3)}  ${note}`);
  } else {
    failures += 1;
    console.log(`  FAIL  ${name.padEnd(width)}  ${problems.join("; ")}`);
  }
}

rmSync(scratch, { recursive: true, force: true });

console.log(
  failures === 0
    ? `\nall ${Object.keys(programs).length} programs agree — wasm module and native compiler`
    : `\n${failures} disagreement(s)`,
);
process.exit(failures === 0 ? 0 : 1);
