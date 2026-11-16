//! EDF+ time-stamped annotation lists (TALs).
//!
//! A TAL is `+onset[\x15duration]\x14text\x14[text\x14...]\0`. An annotation signal block
//! holds TALs one after another, padded with zero bytes.

/// One parsed TAL.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Tal {
    /// Its first byte's position in the block.
    pub start: usize,
    /// Seconds since the start date/time in the header.
    pub onset: f64,
    pub duration: Option<f64>,
    /// May be empty strings (the time-keeping TAL has one empty text).
    pub texts: Vec<String>,
}

/// Parses the TALs of one annotation block. Malformed TALs are skipped and reported in
/// the second return value, so one bad TAL does not hide the rest.
pub(crate) fn parse_block(bytes: &[u8]) -> (Vec<Tal>, Vec<String>) {
    let (mut tals, mut errors) = (Vec::new(), Vec::new());
    let mut rest = bytes;
    loop {
        // Skip padding (and any stray zero bytes between TALs).
        let start = rest.iter().position(|&b| b != 0).unwrap_or(rest.len());
        rest = &rest[start..];
        if rest.is_empty() {
            break;
        }
        let at = bytes.len() - rest.len();
        let (tal, next) = match rest.iter().position(|&b| b == 0) {
            Some(end) => (&rest[..end], &rest[end + 1..]),
            None => {
                errors.push("TAL without terminating zero byte".to_owned());
                (rest, &rest[rest.len()..])
            }
        };
        match parse_tal(tal) {
            Ok(t) => tals.push(Tal { start: at, ..t }),
            Err(e) => errors.push(e),
        }
        rest = next;
    }
    (tals, errors)
}

fn parse_tal(tal: &[u8]) -> Result<Tal, String> {
    let shown = || String::from_utf8_lossy(tal).escape_debug().to_string();
    let mut parts = tal.split(|&b| b == 0x14);
    let time = parts.next().unwrap_or_default();
    let texts: Vec<&[u8]> = parts.collect();
    // Every text ends with \x14, so the last part after splitting is empty.
    let Some((&[], texts)) = texts.split_last() else {
        return Err(format!("TAL \"{}\" does not end with \\x14", shown()));
    };
    let (onset, duration) = match time.iter().position(|&b| b == 0x15) {
        Some(i) => (&time[..i], Some(&time[i + 1..])),
        None => (time, None),
    };
    let onset = match onset.first() {
        Some(b'+' | b'-') => number(onset),
        _ => None,
    }
    .ok_or_else(|| format!("TAL \"{}\" has an invalid onset", shown()))?;
    let duration = match duration {
        None | Some([]) => None,
        Some(d) => Some(
            number(d)
                .filter(|&d| d >= 0.0 && d.is_finite())
                .ok_or_else(|| format!("TAL \"{}\" has an invalid duration", shown()))?,
        ),
    };
    Ok(Tal {
        start: 0,
        onset,
        duration,
        texts: texts
            .iter()
            .map(|t| String::from_utf8_lossy(t).into_owned())
            .collect(),
    })
}

/// A finite decimal number. EDF+ allows only digits and a point, plus a sign for onsets.
fn number(bytes: &[u8]) -> Option<f64> {
    let digits = match bytes.first() {
        Some(b'+' | b'-') => &bytes[1..],
        _ => bytes,
    };
    if digits.is_empty() || !digits.iter().all(|b| b.is_ascii_digit() || *b == b'.') {
        return None;
    }
    std::str::from_utf8(bytes)
        .ok()?
        .parse::<f64>()
        .ok()
        .filter(|x| x.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tal(start: usize, onset: f64, duration: Option<f64>, texts: &[&str]) -> Tal {
        Tal {
            start,
            onset,
            duration,
            texts: texts.iter().map(|t| (*t).to_owned()).collect(),
        }
    }

    #[test]
    fn parses_time_keeping_and_annotations() {
        let block =
            b"+0.25\x14\x14\0+1.5\x152.5\x14sleep\x14N2 \xc2\xb5V\x14\0-3\x14ev\x14\0\0\0\0";
        let (tals, errors) = parse_block(block);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            tals,
            [
                tal(0, 0.25, None, &[""]),
                tal(8, 1.5, Some(2.5), &["sleep", "N2 \u{b5}V"]),
                tal(31, -3.0, None, &["ev"]),
            ]
        );
    }

    #[test]
    fn empty_duration_is_none() {
        let (tals, _) = parse_block(b"+7\x15\x14x\x14\0");
        assert_eq!(tals, [tal(0, 7.0, None, &["x"])]);
    }

    #[test]
    fn skips_malformed_tals_and_keeps_the_rest() {
        let block = b"+1\x14a\x14\0\
                      1\x14no sign\x14\0\
                      +x\x14bad onset\x14\0\
                      +2\x15-1\x14negative duration\x14\0\
                      +3\x14no final separator\0\
                      +4\x14b\x14\0\
                      +5\x14unterminated\x14";
        let (tals, errors) = parse_block(block);
        assert_eq!(
            tals,
            [
                tal(0, 1.0, None, &["a"]),
                tal(78, 4.0, None, &["b"]),
                tal(84, 5.0, None, &["unterminated"])
            ]
        );
        assert_eq!(errors.len(), 5, "{errors:?}");
    }

    #[test]
    fn blank_block_has_no_tals() {
        assert_eq!(parse_block(&[0; 16]), (vec![], vec![]));
        assert_eq!(parse_block(&[]), (vec![], vec![]));
    }
}
