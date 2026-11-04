# BatchCalc

Native batch calculation for engineering Excel workbooks. A workbook whose VBA macro loops
over positions × load cases (setting driver cells, `Application.Calculate`, reading results)
is calculated by a multithreaded Rust engine that evaluates every formula for **all cases at
once**, then writes the results into the workbook exactly as the macro would.

777P2F Frame STA403: ~4 h in Excel → ~3 s calculation, 15–30 s end to end in Excel,
results identical to the macro's (see `docs/VERIFICATION.md`).

## Layout

| Path | Content |
|---|---|
| `crates/xlmodel` | Streaming OOXML (xlsx/xlsm) reader: values, shared/array/dynamic formulas, names |
| `crates/xlformula` | Excel formula parser; relative references stored as offsets (one AST per filled range) |
| `crates/xlcalc` | Lazy, memoised, case-vectorised evaluator; ~150 Excel functions incl. dynamic arrays, LET, lookups, VBA UDF translations |
| `crates/batchcalc` | Contract runner (stages, jobs, cases, overlays), verification, CLI `batchcalc` |
| `crates/xll` | Excel add-in `BatchCalc.xll` (C API): menu, snapshot, background run, write-back |
| `crates/setup` | Per-user installer `BatchCalc-Setup.exe` (no administrator) |
| `contracts/` | Workbook contracts (`fuselage_frame.json`) — see `docs/CONTRACTS.md` |
| `tools/` | Excel oracle/probe/end-to-end test scripts, workbook profiler, release build |
| `docs/` | User guide (RU), contract format, verification report |

## Build

Requirements: Rust (MSVC toolchain), Windows SDK. Then:

```
powershell -ExecutionPolicy Bypass -File tools\Build-Release.ps1
```

produces `dist\BatchCalc-Setup-<version>.exe` (single-file installer) and
`dist\BatchCalc-<version>-portable.zip`. Tests: `cargo test --workspace`.

Workbooks and their derived data are confidential and are never part of the repository or
the release (`.gitignore` excludes Excel files).
