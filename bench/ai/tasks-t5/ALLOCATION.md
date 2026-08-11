# tier-5 tasks — frozen observation-point allocation (design/0011 §1)

Six constrained-integration tasks in two families. Frozen before any model
run; the commit hash is the proof the tasks were not tuned to the runs.
Family split and the allocation below follow design/0011 §1 exactly.

| Task | Family | Target file | Observation point |
| --- | --- | --- | --- |
| t5-01 | t5a implementation graft | `src/manifest.xn` | **XN7008 temptation.** `depot.locker.Locker.weight` is a `var` field, so assigning it directly looks like the shortest path — but the private capacity rule lives behind `depot.locker.stow`, so the only path to the expected stdout never crosses the module boundary. Recorded per 0011 §1: the correct path is reachable without any boundary crossing, and the crossing is staged to *look* shortest. |
| t5-02 | t5a implementation graft | `src/watchlog.xn` | **Exhaustive match over a foreign pub enum.** `harbor.signal.Signal` has three variants, two carrying payloads; the model's file must match all of them through fully qualified names. |
| t5-03 | t5a implementation graft | `src/tally.xn` | **Result chain + effect declarations spanning files.** `relay.feed.emit` performs `Io.write`; the model's `tally.publish` must declare the effect, thread `io`, and sit in the `?` chain between the provided emitter and the frozen `main`. |
| t5-04 | t5a implementation graft | `src/cutlist.xn` | General constrained integration: two provided modules with a one-way dependency (`mill.rules` uses `mill.stock`); no single scripted trap. |
| t5-05 | t5b wiring | `src/main.xn` | **use/wiring pathway** (XN2007 / XN2002 use-fix territory). No entry file provided; the model authors `use`, fully qualified references, the effect declaration and `fn main` placement, splitting output between a provided effectful `serve` and separators written by `main`. |
| t5-06 | t5b wiring | `src/main.xn` | **use/wiring pathway** (XN2007 / XN2002 use-fix territory). No entry file provided; a provided transformer (`stable.herd.feed`) feeds a provided renderer (`stable.ledger.entry`) across two modules. |

Shared rules, per 0011 §1 and §3:

- Skeletons (`skeleton/`) are frozen: `xenith.toml`, provided modules, and —
  in t5a only — `src/main.xn`, which is the calling contract and is the one
  skeleton text a prompt may carry. **Provided module sources never enter any
  prompt.**
- Hidden test is exact stdout from `xenith run` on the assembled project.
- Every reference solution (`solution.xn`) consumes the provided modules'
  pub API rather than reimplementing it, and each task statement says so;
  the data each output depends on (locker capacity, the signal sequence, the
  frame list, the inventory, the board, the lineup) lives only in the
  provided modules, so a hardcoded output cannot be derived from the prompt.
- `api-dump.txt` in each task directory is the frozen machine-generated API
  dump for the `api` arms (design/0011 §7), produced by
  `xenith-bench api-dump <task>/skeleton` — never hand-edited. `verify`
  regenerates it and fails on any drift, and gates that every provided
  surface the reference consumes appears in the dump.

Amendment rule (0011 §5): if a frozen task turns out to be wrong once
`verify` or the pilot runs, the fix lands as its own commit explaining the
error — amendments stay visible, never silent.
