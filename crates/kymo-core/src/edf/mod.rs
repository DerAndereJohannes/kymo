//! EDF, EDF+, BDF and BDF+ files.

mod header;

pub use header::{Block, EdfError, ParsedHeader, RecordLayout, parse_header};
