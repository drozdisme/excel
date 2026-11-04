# Contracts: describing a workbook's batch calculation

A contract (`contracts/*.json`) is the declarative equivalent of a workbook's VBA case loop.
BatchCalc never guesses: the contract states which cells drive the calculation, which cells
are results and where the macro writes them. All expressions are **Excel formulas** evaluated
by the engine itself; they may use the variables below.

| Name | Meaning |
|---|---|
| loop variables (`I`, `K`, ...) | Current job, from `loops` |
| job variables (`ID`, `LOADSET`, ...) | From `vars`, evaluated in order per job |
| `J` | Case number 1..N (one lane per case) |
| `CASE_ID` | Value of `cases.id` for the case |
| `CASE_COUNT` | N |

Use absolute, sheet-qualified references (`Node_Map!$B:$B`).

## Structure

```jsonc
{
  "name": "...", "description": "...",
  "required_sheets": ["..."],              // the contract applies when all exist
  "commands": [{ "name": "all", "label": "Run All Analysis", "stages": ["sections", "joints"] }],
  "stages": [{
    "name": "sections",
    "freeze": ["Sheet"],                   // Worksheet.EnableCalculation = False during the stage
    "clear":  ["Sheet", "Sheet!A1:B2"],    // ClearContents before writing
    "set":    [["VBA_Config!F4", "\"Y\""]],// cells assigned for the stage
    "after":  [["VBA_Config!F4", "\"N\""]],// cells left behind
    "loops":  [{ "var": "I", "from": "1", "to": "100" }],   // nested job loops
    "vars":   [["ID", "INDEX(Node_Map!$B:$B,55+I)&\"\""]],
    "skip":       "ID=\"\"",               // job does nothing
    "skip_cases": "FLAG=1",                // job writes headers but no case results
    "cases":  { "count": "Loadcases!$C$1", "id": "INDEX(Frame_Loads!$2:$2,16+3*(J-1))&\"\"" },
    "drivers":[{ "cell": "Section_Analysis!K60", "value": "ID" },
               { "cell": "Section_Analysis!K62", "value": "CASE_ID", "per_case": true }],
    "outputs":[{ "source": "Section_Analysis!D59:D70", "sheet": "MS",
                 "row": "((I-1)*2+(K-1))*12+5", "col": "5",
                 "step": [1, 0],          // offset per source cell
                 "case_step": [0, 1],     // offset per case; omit for case-independent values
                 "when": "K=1", "ignore_skip_cases": false }],
    "headers":[{ "scope": "static|job|case", "sheet": "MS", "row": "3", "col": "2",
                 "values": ["\"Stringer\"", "\"Load set\""], "step": [0, 1], "case_step": [0, 1] }]
  }]
}
```

Semantics follow the VBA macro exactly:

- Driver values and flags left by one stage are the workbook state of the next stage
  (e.g. the shear-tie stage sees `K61/K62` as the joint loop left them).
- Values written by a stage (outputs, headers, clears) are visible to later stages.
- Writes follow `Range.Value2 = array`: empty text becomes an empty cell, text stays text.
- An output without `case_step` must not depend on the case; otherwise the run stops.

## Writing a contract for a new template

1. Read the macro: find the loops, the cells it assigns before `Application.Calculate`,
   the cells it reads afterwards and where the arrays are written at the end.
2. Translate each into the fields above (the Frame contract is a complete example).
3. Verify against results the macro saved in the workbook:
   `batchcalc run BOOK.xlsm --contract new.json --command all --verify`
   and on new inputs against real Excel (`tools/Capture-Oracle.ps1` + `batchcalc selfcheck --oracle`).
4. Copy the contract next to `BatchCalc.xll` (`contracts` folder) or into
   `%APPDATA%\BatchCalc\contracts`; restart Excel.
