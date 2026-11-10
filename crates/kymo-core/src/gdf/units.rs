//! GDF 2's physical dimension codes: a unit in the upper 11 bits, a decimal prefix in the
//! lower 5 (biosig's `units.csv` and `DecimalFactors.txt`).

/// Units of the codes biosignals use, by `code & !31`. Labels are written as
/// [`Unit::from_label`](crate::Unit::from_label) reads them where it knows the unit.
const UNITS: &[(u16, &str)] = &[
    (512, ""), // dimensionless
    (544, "%"),
    (576, "ppht"),
    (608, "ppm"),
    (736, "degree"),
    (768, "rad"),
    (1088, "beat"),
    (1120, "breath"),
    (1280, "m"),
    (1600, "l"),
    (1728, "g"),
    (2176, "s"),
    (2208, "min"),
    (2464, "1/s"),
    (2496, "Hz"),
    (2528, "1/min"),
    (2720, "bpm"),
    (2784, "resp/min"),
    (2816, "m/s"),
    (3072, "l/min"),
    (3744, "Ns"),
    (3776, "N"),
    (3840, "Pa"),
    (3872, "mmHg"),
    (3904, "cmH2O"),
    (3936, "bar"),
    (3968, "J"),
    (4032, "W"),
    (4160, "A"),
    (4192, "C"),
    (4256, "V"),
    (4288, "Ohm"),
    (4352, "F"),
    (4384, "K"),
    (4480, "cd"),
    (4544, "mol"),
    (6048, "\u{b0}C"),
    (6432, "B"),
    (6624, "m/s\u{b2}"),
    (7776, "Boolean"),
    (7808, "dB(V)"),
    (8096, "rpm"),
    (8256, "V/s"),
    (8288, "S"),
    (9056, "a.u."),
    (65504, "T"),
];

/// Decimal prefixes by `code & 31`; the others are undefined.
fn prefix(code: u16) -> Option<&'static str> {
    Some(match code & 31 {
        0 => "",
        1 => "da",
        2 => "h",
        3 => "k",
        4 => "M",
        5 => "G",
        6 => "T",
        7 => "P",
        8 => "E",
        9 => "Z",
        10 => "Y",
        16 => "d",
        17 => "c",
        18 => "m",
        19 => "\u{b5}",
        20 => "n",
        21 => "p",
        22 => "f",
        23 => "a",
        24 => "z",
        25 => "y",
        _ => return None,
    })
}

/// The unit label of a physical dimension code; `None` for 0 (no code; the label then
/// comes from the obsolete text field) and for codes outside the table.
pub(super) fn label(code: u16) -> Option<String> {
    if code == 0 {
        return None;
    }
    let unit = UNITS.iter().find(|(c, _)| *c == code & !31)?.1;
    let prefix = prefix(code)?;
    if unit.is_empty() && !prefix.is_empty() {
        return None;
    }
    Some(format!("{prefix}{unit}"))
}
