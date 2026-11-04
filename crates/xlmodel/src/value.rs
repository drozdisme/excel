use std::fmt;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum XlError {
    Null,
    Div0,
    Value,
    Ref,
    Name,
    Num,
    NA,
    GettingData,
    Spill,
    Calc,
    Field,
    Connect,
    Blocked,
    Unknown,
    Busy,
    Python,
}

impl XlError {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "#NULL!" => Self::Null,
            "#DIV/0!" => Self::Div0,
            "#VALUE!" => Self::Value,
            "#REF!" => Self::Ref,
            "#NAME?" => Self::Name,
            "#NUM!" => Self::Num,
            "#N/A" => Self::NA,
            "#GETTING_DATA" => Self::GettingData,
            "#SPILL!" => Self::Spill,
            "#CALC!" => Self::Calc,
            "#FIELD!" => Self::Field,
            "#CONNECT!" => Self::Connect,
            "#BLOCKED!" => Self::Blocked,
            "#UNKNOWN!" => Self::Unknown,
            "#BUSY!" => Self::Busy,
            "#PYTHON!" => Self::Python,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Null => "#NULL!",
            Self::Div0 => "#DIV/0!",
            Self::Value => "#VALUE!",
            Self::Ref => "#REF!",
            Self::Name => "#NAME?",
            Self::Num => "#NUM!",
            Self::NA => "#N/A",
            Self::GettingData => "#GETTING_DATA",
            Self::Spill => "#SPILL!",
            Self::Calc => "#CALC!",
            Self::Field => "#FIELD!",
            Self::Connect => "#CONNECT!",
            Self::Blocked => "#BLOCKED!",
            Self::Unknown => "#UNKNOWN!",
            Self::Busy => "#BUSY!",
            Self::Python => "#PYTHON!",
        }
    }
    /// ERROR.TYPE code.
    pub fn code(self) -> u8 {
        match self {
            Self::Null => 1,
            Self::Div0 => 2,
            Self::Value => 3,
            Self::Ref => 4,
            Self::Name => 5,
            Self::Num => 6,
            Self::NA => 7,
            Self::GettingData => 8,
            Self::Spill => 9,
            Self::Connect => 10,
            Self::Blocked => 11,
            Self::Unknown => 12,
            Self::Field => 13,
            Self::Calc => 14,
            Self::Busy => 15,
            Self::Python => 16,
        }
    }
}

impl fmt::Display for XlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stored cell value. `Empty` (never written) is distinct from `Text("")`.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum CellValue {
    #[default]
    Empty,
    Number(f64),
    Text(Arc<str>),
    Bool(bool),
    Error(XlError),
}
