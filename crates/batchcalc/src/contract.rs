//! Declarative description of a workbook's batch calculation (what the VBA loop does).
//! Every expression is an Excel formula; loop variables, job variables, `J` (case index,
//! one lane per case) and `CASE_ID` are available as names.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Sheets that must exist for the contract to apply to a workbook.
    pub required_sheets: Vec<String>,
    /// Optional extra identification: formulas that must evaluate to TRUE.
    #[serde(default)]
    pub identify: Vec<String>,
    pub stages: Vec<Stage>,
    /// User-facing commands (button equivalents), each a list of stage names in order.
    #[serde(default)]
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub name: String,
    pub label: String,
    pub stages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub name: String,
    /// Sheets whose formulas keep their stored values during the stage (EnableCalculation=False).
    #[serde(default)]
    pub freeze: Vec<String>,
    /// Sheets ("Sheet") or ranges ("Sheet!A1:B2") cleared before the stage writes.
    #[serde(default)]
    pub clear: Vec<String>,
    /// Cells assigned during the stage: [cell, formula].
    #[serde(default)]
    pub set: Vec<[String; 2]>,
    /// Cells left in the workbook after the stage: [cell, formula].
    #[serde(default)]
    pub after: Vec<[String; 2]>,
    /// Nested job loops, outermost first.
    pub loops: Vec<Loop>,
    /// Job variables in evaluation order: [name, formula].
    #[serde(default)]
    pub vars: Vec<[String; 2]>,
    /// Job produces nothing when TRUE.
    #[serde(default)]
    pub skip: Option<String>,
    /// Job writes headers but no case outputs when TRUE.
    #[serde(default)]
    pub skip_cases: Option<String>,
    #[serde(default)]
    pub cases: Option<Cases>,
    pub drivers: Vec<Driver>,
    pub outputs: Vec<Output>,
    #[serde(default)]
    pub headers: Vec<Header>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Loop {
    pub var: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cases {
    /// Number of cases.
    pub count: String,
    /// Case identifier for lane `J` (1-based).
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Driver {
    pub cell: String,
    pub value: String,
    /// Value differs per case (evaluated over all lanes).
    #[serde(default)]
    pub per_case: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    /// Source range on the calculation sheet; cells are taken row-major as index `o`.
    pub source: String,
    pub sheet: String,
    /// Target of source cell 0, case 1 (formulas, evaluated per job).
    pub row: String,
    pub col: String,
    /// Target offset per source cell [rows, cols].
    #[serde(default = "down")]
    pub step: [i32; 2],
    /// Target offset per case [rows, cols]; `None` = case-independent value written once.
    #[serde(default)]
    pub case_step: Option<[i32; 2]>,
    #[serde(default)]
    pub when: Option<String>,
    /// Written even when the job's `skip_cases` is TRUE.
    #[serde(default)]
    pub ignore_skip_cases: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Evaluated once per stage.
    Static,
    /// Evaluated per job (after drivers are set).
    Job,
    /// Evaluated per case (one lane per case), once per stage.
    Case,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub scope: Scope,
    pub sheet: String,
    pub row: String,
    pub col: String,
    /// Values written at successive `step` offsets.
    pub values: Vec<String>,
    #[serde(default = "right")]
    pub step: [i32; 2],
    #[serde(default)]
    pub case_step: Option<[i32; 2]>,
    #[serde(default)]
    pub when: Option<String>,
}

fn down() -> [i32; 2] {
    [1, 0]
}
fn right() -> [i32; 2] {
    [0, 1]
}

impl Contract {
    pub fn from_json(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| format!("contract: {e}"))
    }
    pub fn stage(&self, name: &str) -> Option<&Stage> {
        self.stages.iter().find(|s| s.name.eq_ignore_ascii_case(name))
    }
}
