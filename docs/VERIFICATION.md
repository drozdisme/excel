# Verification — 777P2F Frame STA403 (RevH1)

Reference: the supplied workbook, whose output sheets were produced by the original VBA
macros (Frame, Joint and Shear Tie analysis). Machine: Intel i5-14600KF (14 cores / 20
threads), 64 GB, Windows 10, Excel 16.0.20326 x64.

| Check | Scope | Result |
|---|---|---|
| Engine run vs saved macro outputs (`batchcalc run --command all --verify`) | 15,937,582 cells in 9 output sheets incl. headers, text, errors and blanks | **0 different**, max \|Δ\| 4.9e-11 |
| Engine recalculation of every formula vs values stored by Excel (`selfcheck`) | 91,413 formula cells, 37 sheets | **0 different** |
| Engine vs fresh real-Excel recalculation at another driver state (`Capture-Oracle` + `selfcheck --oracle`) | 11,236 cells of Shear_Tie_Analysis at location TCL-L | **0 different** |
| Add-in in real Excel, then workbook saved and compared with the original (`tools/Test-AddIn.ps1` + `batchcalc diff`) | all 37 sheets, ~17.6 M cells (reports recalculated by Excel itself) | **EQUAL** |

Defects found during verification and fixed in the engine (each confirmed against real Excel):

1. Dynamic-array spill size depends on the inputs; the file only records the last one.
   The engine now determines spill ranges at run time (and reports `#SPILL!` when blocked).
2. A scalar error argument makes `HSTACK`/`VSTACK` return that error (not an array with an
   error element).
3. `INDEX` with a negative row/column returns `#VALUE!`, beyond the range `#REF!`.
4. Errors newer than the OOXML set (`#SPILL!`, `#CALC!`, ...) are stored in files as `#VALUE!`.

## PD template V6 (.xlsb)

The outputs saved in this workbook come from a macro run on 8 Apr 2026; its inputs were
changed afterwards (Excel itself now computes different values), so they are **not** a valid
reference. Verification is against live Excel instead:

| Check | Scope | Result |
|---|---|---|
| Native .xlsb reader vs .xlsx (same workbook saved in both formats) | 90,912 formulas, 346 names, 18.8 M values | equivalent (only cells Excel itself recalculated differently during conversion) |
| Frame workbook run from its .xlsb copy (`--verify`) | 15,936,471 cells | **0 different** |
| Engine recalculation of the PD calculation sheets vs stored values | 34,732 formula cells | **0 different** |
| SkinAnalysis (21,485 cells incl. the data-table output column) vs fresh Excel at 7 bay/load-case points | 7 × 21,485 cells | **0 different** |

| Add-in in live Excel on the .xlsb (Run Analysis), saved, then engine re-run on the saved book with `--verify` | 57,331,260 written cells | **0 different** |
| Summary formulas recalculated by Excel after the write vs engine (`selfcheck`) | 91,873 formula cells | **0 different** |

Live Excel end to end: 86 s from the menu command to the finished workbook (snapshot 15 s,
calculation 11 s, writing 57.3 M cells and Excel's own report recalculation ~60 s).
The original macro run recorded in the workbook took 2 h 26 min.

Additional defects found and fixed: names are evaluated as array formulas; FILTER with a
case-dependent condition; xlsb shared formulas name their master's column in the extra
data (overlapping shared ranges).

## Performance

| Stage (all 2,323 load cases) | Time |
|---|---|
| Read workbook (inputs only) | 0.9 s |
| Sections: 192 location/load-set jobs | 2.4 s |
| Joints: 98 jobs | 0.1 s |
| Shear ties: 100 jobs | 0.1 s |
| Add-in: snapshot + calculation + writing 15.9 M cells into Excel | 13–28 s end to end |

Original macro: about 4 hours. Earlier Python prototype: 1,434 s calculation + 191 s import.

## Reproducing

```
batchcalc run BOOK.xlsm --command all --verify
batchcalc selfcheck BOOK.xlsm
powershell tools/Capture-Oracle.ps1 -Workbook BOOK.xlsm -Set 'Section_Analysis!K60=TCL-L' -Sheets Shear_Tie_Analysis -Out o.tsv
batchcalc selfcheck BOOK.xlsm --sheets Shear_Tie_Analysis --set Section_Analysis!K60=TCL-L --oracle o.tsv
powershell tools/Test-AddIn.ps1 -Workbook BOOK.xlsm -Xll BatchCalc.xll -AllowMacros -ClearSheets @()
batchcalc diff BOOK.xlsm %TEMP%\BatchCalcTest\result.xlsm --sheets <all sheets>
```
