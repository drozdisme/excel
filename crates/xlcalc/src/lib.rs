//! Lane-vectorised Excel formula evaluation: one formula graph, many cases at once.

pub mod eval;
mod format;
mod funcs;
mod funcs_array;
mod funcs_ref;
mod funcs_stat;
pub mod lanes;
pub mod lookup;
pub mod scalar;
mod scalars;
mod udf;
pub mod value;

pub use eval::{Ctx, EvalError, Job, Model};
pub use lanes::Lanes;
pub use scalar::Scalar;
pub use value::{Array, RefRange, Value};
pub use eval::{Overlay, SheetPatch};
