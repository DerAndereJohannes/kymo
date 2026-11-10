//! EDF, EDF+, BDF and BDF+ files.

pub(crate) mod header;
mod index;
mod reader;
mod tal;

pub use header::{Block, EdfError, ParsedHeader, RecordLayout, parse_header};
pub use reader::open;
