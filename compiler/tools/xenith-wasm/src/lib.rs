//! The whole compiler, in one WebAssembly module.
//!
//! This is not a backend. It is the *existing* pipeline — parse, check, run —
//! behind one function that takes source text and returns what the CLI would
//! have printed. There is no second implementation of Xenith here and there is
//! no code generation: `wasm32-unknown-unknown` is a host the interpreter runs
//! *on*, not a target it emits (design/0018).
//!
//! Two facts make that cheap:
//!
//! - `xenith-syntax`, `xenith-sema`, `xenith-vm` and `xenith-diag` touch no
//!   host service at all, so they cross-compile unchanged.
//! - The browser has no threads on `wasm32-unknown-unknown`, and it does not
//!   need any: design/0017 §5 keeps the **sequential executor** as the
//!   differential oracle and the compiler's main gate asserts that both
//!   executors produce byte-identical stdout, exit codes and diagnostics. So
//!   [`EXECUTOR`] below is a deployment choice, not a semantic one.
//!
//! # The ABI
//!
//! Deliberately raw — no `wasm-bindgen`, no build step beyond `cargo build`,
//! no npm. The module exports its linear `memory`, plus:
//!
//! | Export | Meaning |
//! | --- | --- |
//! | `xn_alloc(len) -> ptr` | reserve `len` bytes for the caller to write UTF-8 source into |
//! | `xn_run(ptr, len) -> ptr` | consume that buffer, return a result buffer |
//! | `xn_free(ptr, len)` | release a buffer returned by `xn_run` |
//!
//! A result buffer is a little-endian `u32` byte length followed by that many
//! bytes of UTF-8 JSON, so one pointer carries everything back.

use xenith_diag::LineIndex;
use xenith_vm::Executor;

/// Which executor the module runs.
///
/// `wasm32-unknown-unknown` has no threads; `std::thread::Builder::spawn`
/// fails there, and `xenith_vm` would fall back to this executor on its own.
/// Naming it is better than relying on that: design/0017 §5 makes
/// `Sequential` the oracle the shipped parallel executor is tested against
/// byte for byte, so a browser run is a conforming run, not a lenient one.
const EXECUTOR: Executor = Executor::Sequential;

/// The file name the playground reports diagnostics against. The web page has
/// no file system, but a diagnostic without a file is a shape change, and the
/// wire format is an interface (`xenith-driver::wire`).
const PLAYGROUND_FILE: &str = "playground.xn";

/// Everything one run of a Xenith program produced.
///
/// The fields mirror what `xenith run <file>` puts on stdout, stderr and the
/// exit status — the same three channels, structured instead of interleaved.
#[derive(Debug)]
pub struct RunResult {
    /// 0 = `main` returned `Ok`; 1 = `Err`; 2 = refused, the file has
    /// diagnostics; 101 = a runtime trap (spec/04 §5).
    pub exit: i32,
    /// Exactly the bytes `io.write` produced, decoded lossily for transport.
    pub stdout: String,
    /// The trap, when `exit` is 101.
    pub error: Option<RunError>,
    /// One file's worth of diagnostics in the shared wire shape
    /// (`xenith_driver::wire::file_diagnostics`) — empty array when clean.
    pub diagnostics: serde_json::Value,
}

/// A runtime trap, positioned the way the CLI positions it.
#[derive(Debug)]
pub struct RunError {
    pub message: String,
    pub line: u32,
    pub column: u32,
}

/// Check `source`, then run it if it is clean.
///
/// This follows `xenith run`'s single-file path step for step: analyse,
/// refuse on any diagnostic, otherwise parse, collect definitions and
/// interpret. A file whose only gaps are holes runs, and reaching a hole is a
/// trap that names it (spec/04 §5).
pub fn run_source(source: &str) -> RunResult {
    let analysis = xenith_driver::analyze_source(source);
    let report =
        xenith_driver::wire::file_diagnostics(PLAYGROUND_FILE, source, &analysis.diagnostics, true);

    // Running a program with diagnostics would be executing guesses.
    if !analysis.diagnostics.is_empty() {
        return RunResult {
            exit: 2,
            stdout: String::new(),
            error: None,
            diagnostics: report,
        };
    }

    let parsed = xenith_syntax::parse(source);
    let (table, _) = xenith_sema::def::collect(&parsed.module);
    let outcome = xenith_vm::run_with(&parsed.module, &table, EXECUTOR);

    let error = outcome.error.map(|(message, span)| {
        let at = LineIndex::new(source).line_col(source, span.start);
        RunError {
            message,
            line: at.line,
            column: at.column,
        }
    });

    RunResult {
        exit: outcome.exit,
        stdout: String::from_utf8_lossy(&outcome.stdout).into_owned(),
        error,
        diagnostics: report,
    }
}

/// [`run_source`], rendered as the JSON the web page reads.
pub fn run_source_json(source: &str) -> String {
    let result = run_source(source);
    let error = match result.error {
        Some(error) => serde_json::json!({
            "message": error.message,
            "line": error.line,
            "column": error.column,
        }),
        None => serde_json::Value::Null,
    };
    serde_json::json!({
        "exit": result.exit,
        "stdout": result.stdout,
        "error": error,
        "report": result.diagnostics,
    })
    .to_string()
}

// ----------------------------------------------------------------- the ABI

/// Reserve `len` bytes of module memory and hand back the pointer.
///
/// The caller writes UTF-8 source into it and passes the same pair to
/// [`xn_run`], which takes ownership.
#[unsafe(no_mangle)]
pub extern "C" fn xn_alloc(len: usize) -> *mut u8 {
    let mut buffer = vec![0u8; len];
    let pointer = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    pointer
}

/// Run the UTF-8 source at `ptr`/`len` and return a result buffer: a
/// little-endian `u32` length, then that many bytes of UTF-8 JSON.
///
/// # Safety
///
/// `ptr`/`len` must be exactly a buffer from [`xn_alloc`], not yet consumed.
/// This call takes ownership of it. The returned buffer belongs to the caller
/// and is released with [`xn_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xn_run(ptr: *mut u8, len: usize) -> *mut u8 {
    let source = unsafe { Vec::from_raw_parts(ptr, len, len) };
    let json = run_source_json(&String::from_utf8_lossy(&source));

    let bytes = json.into_bytes();
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(&bytes);
    let pointer = out.as_mut_ptr();
    std::mem::forget(out);
    pointer
}

/// Release a buffer returned by [`xn_run`].
///
/// # Safety
///
/// `ptr` must come from [`xn_run`] and `len` must be `4 + payload_length`,
/// the total the caller read out of it. Each buffer may be freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn xn_free(ptr: *mut u8, len: usize) {
    drop(unsafe { Vec::from_raw_parts(ptr, len, len) });
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELLO: &str = r#"fn greeting(name: String) -> String {
    "Hello, ".concat(other: name)
}

fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {
    io.write(text: greeting(name: "world"))?;
    return Ok(unit);
}
"#;

    #[test]
    fn a_clean_program_runs() {
        let result = run_source(HELLO);
        assert_eq!(result.exit, 0);
        assert_eq!(result.stdout, "Hello, world");
        assert!(result.error.is_none());
    }

    #[test]
    fn a_program_with_diagnostics_is_refused() {
        let result = run_source(
            "fn main(io: Io) -> Result<Unit, Error> {\n    io.write(text: \"hi\")?;\n    return Ok(unit);\n}\n",
        );
        assert_eq!(result.exit, 2);
        assert_eq!(result.stdout, "");
        let codes = result.diagnostics["diagnostics"]
            .as_array()
            .expect("an array of diagnostics");
        assert!(
            !codes.is_empty(),
            "the missing `uses` clause must be caught"
        );
    }

    #[test]
    fn a_trap_is_positioned() {
        let result = run_source(
            "fn main(io: Io) -> Result<Unit, Error> uses {Io.write} {\n    let n = 1 / 0;\n    io.write(text: n.to_text())?;\n    return Ok(unit);\n}\n",
        );
        assert_eq!(result.exit, 101);
        let error = result.error.expect("a trap");
        assert_eq!(error.message, "division by zero");
        assert_eq!(error.line, 2);
    }

    #[test]
    fn tasks_run_through_the_sequential_executor() {
        let result = run_source(
            "fn plan(n: Int) -> Int {\n    n * 2\n}\n\nfn main(io: Io) -> Result<Unit, Error> uses {Io.write, Task.spawn} {\n    scope {\n        let a = spawn plan(n: 1);\n        let b = spawn plan(n: 2);\n        let total = a.await + b.await;\n        io.write(text: total.to_text())?;\n    }\n    return Ok(unit);\n}\n",
        );
        assert_eq!(result.exit, 0);
        assert_eq!(result.stdout, "6");
    }

    #[test]
    fn the_json_carries_the_same_answer() {
        let json: serde_json::Value =
            serde_json::from_str(&run_source_json(HELLO)).expect("valid JSON");
        assert_eq!(json["exit"], 0);
        assert_eq!(json["stdout"], "Hello, world");
        assert_eq!(json["report"]["file"], PLAYGROUND_FILE);
    }
}
