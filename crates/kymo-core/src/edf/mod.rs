//! EDF, EDF+, BDF and BDF+ files.

mod header;
mod reader;

pub use header::{Block, EdfError, ParsedHeader, RecordLayout, parse_header};
pub use reader::{EdfReader, open};
