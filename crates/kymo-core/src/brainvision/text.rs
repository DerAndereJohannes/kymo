//! The text of BrainVision header and marker files: their codepage, and their INI-like
//! sections of `key=value` lines.

/// Decodes a header or marker file.
///
/// A UTF-8 byte-order mark means UTF-8. Otherwise the `Codepage` key decides: UTF-8,
/// Latin-1 (ISO-8859-1), or Windows-1252 (which Recorder calls "ANSI"). Without one, or with
/// one this doesn't know, the text is UTF-8 if it is valid UTF-8 and Windows-1252 otherwise,
/// as older files are. Text that isn't valid in its stated codepage is read as Windows-1252
/// with a warning.
pub(super) fn decode(bytes: &[u8], what: &str, warnings: &mut Vec<String>) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xef\xbb\xbf") {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let codepage = codepage(bytes);
    let utf8 = std::str::from_utf8(bytes).ok();
    let label = codepage.as_deref().map(str::to_ascii_lowercase);
    match label.as_deref() {
        Some("utf-8" | "utf8") => match utf8 {
            Some(text) => text.to_owned(),
            None => {
                warnings.push(format!(
                    "the {what} is not valid UTF-8, as its codepage says; read as Windows-1252"
                ));
                windows_1252(bytes)
            }
        },
        Some("latin-1" | "latin1" | "iso-8859-1" | "iso8859-1") => latin_1(bytes),
        Some("ansi" | "windows-1252" | "cp1252") => windows_1252(bytes),
        other => {
            if other.is_some() {
                warnings.push(format!(
                    "the {what} has the unknown codepage {:?}; read as UTF-8 or Windows-1252",
                    codepage.unwrap_or_default()
                ));
            }
            utf8.map_or_else(|| windows_1252(bytes), str::to_owned)
        }
    }
}

/// The value of the first `Codepage=` line, read as ASCII (codepage names are).
fn codepage(bytes: &[u8]) -> Option<String> {
    bytes.split(|&b| b == b'\n').find_map(|line| {
        let line = line.trim_ascii();
        let (key, value) = line.split_at(line.iter().position(|&b| b == b'=')?);
        key.trim_ascii()
            .eq_ignore_ascii_case(b"codepage")
            .then(|| String::from_utf8_lossy(value[1..].trim_ascii()).into_owned())
    })
}

fn latin_1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| char::from(b)).collect()
}

/// Windows-1252: Latin-1 except for 0x80 to 0x9F. The five bytes it leaves undefined keep
/// their Latin-1 meaning (as the WHATWG encoding standard has it).
fn windows_1252(bytes: &[u8]) -> String {
    const HIGH: [char; 32] = [
        '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}',
        '\u{8f}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}',
        '\u{2014}', '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}',
        '\u{178}',
    ];
    bytes
        .iter()
        .map(|&b| match b {
            0x80..=0x9f => HIGH[usize::from(b - 0x80)],
            _ => char::from(b),
        })
        .collect()
}

/// Sections that end a `[Comment]` section, whose free text may hold anything else.
const SECTIONS: [&str; 8] = [
    "common infos",
    "binary infos",
    "ascii infos",
    "channel infos",
    "coordinates",
    "channel user infos",
    "marker infos",
    "marker user infos",
];

/// The parsed lines of a header or marker file.
#[derive(Debug, Default)]
pub(super) struct Ini {
    /// The identification line, e.g. "Brain Vision Data Exchange Header File Version 1.0".
    pub(super) first_line: String,
    /// Comment lines (`;`) before the first section, without the `;`.
    pub(super) preamble: Vec<String>,
    /// Sections in file order; names as written.
    sections: Vec<(String, Vec<(String, String)>)>,
    /// The free text of the `[Comment]` section.
    pub(super) comment: String,
}

impl Ini {
    /// Parses `text`: `[Section]` lines, `key=value` lines (both trimmed), `;` comments.
    /// Lines of another shape are ignored. Everything from `[Comment]` up to the next known
    /// section is free text.
    pub(super) fn parse(text: &str) -> Ini {
        let mut ini = Ini::default();
        let mut lines = text.lines();
        ini.first_line = lines.next().unwrap_or_default().trim().to_owned();
        let mut in_comment = false;
        for line in lines {
            let trimmed = line.trim();
            if let Some(name) = trimmed
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
            {
                let lower = name.trim().to_ascii_lowercase();
                if lower == "comment" {
                    in_comment = true;
                    continue;
                }
                if !in_comment || SECTIONS.contains(&lower.as_str()) {
                    in_comment = false;
                    ini.sections.push((name.trim().to_owned(), Vec::new()));
                    continue;
                }
            }
            if in_comment {
                ini.comment.push_str(line);
                ini.comment.push('\n');
                continue;
            }
            if let Some(comment) = trimmed.strip_prefix(';') {
                if ini.sections.is_empty() {
                    ini.preamble.push(comment.trim().to_owned());
                }
                continue;
            }
            if let (Some((_, entries)), Some((key, value))) =
                (ini.sections.last_mut(), trimmed.split_once('='))
            {
                entries.push((key.trim().to_owned(), value.trim().to_owned()));
            }
        }
        ini
    }

    /// The entries of the first section called `name` (ignoring case), in file order.
    pub(super) fn section(&self, name: &str) -> &[(String, String)] {
        self.sections
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map_or(&[], |(_, entries)| entries)
    }

    /// The value of the first `key` (ignoring case) in section `section`.
    pub(super) fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.section(section)
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

/// Turns BrainVision's escaped commas (`\1`) back into commas.
pub(super) fn unescape(field: &str) -> String {
    field.replace("\\1", ",")
}
