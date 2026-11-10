//! A small writer of MATLAB's level 5 MAT-files (MATLAB's `-v6` format, as EEGLAB saves
//! `.set` files), for the EEGLAB writer and for tests that need MAT-files of their own.
//!
//! Doubles whose values are all whole numbers are stored in the smallest integer type that
//! holds them, as MATLAB stores them (`srate` as uint8, say); data elements of up to 4
//! bytes are packed into their tag, as MATLAB packs them.

/// Byte order of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    Little,
    Big,
}

/// Data element types.
pub const MI_INT8: u32 = 1;
pub const MI_UINT8: u32 = 2;
pub const MI_INT16: u32 = 3;
pub const MI_UINT16: u32 = 4;
pub const MI_INT32: u32 = 5;
pub const MI_UINT32: u32 = 6;
pub const MI_SINGLE: u32 = 7;
pub const MI_DOUBLE: u32 = 9;
pub const MI_INT64: u32 = 12;
pub const MI_UINT64: u32 = 13;
pub const MI_MATRIX: u32 = 14;
pub const MI_COMPRESSED: u32 = 15;
pub const MI_UTF8: u32 = 16;

/// Array classes.
pub const MX_CELL: u8 = 1;
pub const MX_STRUCT: u8 = 2;
pub const MX_CHAR: u8 = 4;
pub const MX_SPARSE: u8 = 5;
pub const MX_DOUBLE: u8 = 6;
pub const MX_SINGLE: u8 = 7;
pub const MX_INT32: u8 = 12;
pub const MX_UINT8: u8 = 9;
pub const MX_FUNCTION: u8 = 16;

/// A MATLAB array.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// Doubles of the given dimensions, column-major.
    Double(Vec<usize>, Vec<f64>),
    /// Singles of the given dimensions, column-major.
    Single(Vec<usize>, Vec<f32>),
    /// Numeric data written as given: the class, the storage type and its bytes, already
    /// in the file's byte order.
    Raw {
        class: u8,
        storage: u32,
        dims: Vec<usize>,
        bytes: Vec<u8>,
    },
    /// A 1 × n char array, stored as UTF-16 as MATLAB stores it.
    Char(String),
    /// A char array of several rows, padded with blanks to the longest.
    Rows(Vec<String>),
    /// A struct array: dimensions, field names, and each element's fields in that order.
    Struct(Vec<usize>, Vec<String>, Vec<Vec<Value>>),
    Cell(Vec<usize>, Vec<Value>),
    Logical(Vec<bool>),
    /// A class kymo skips (a function handle, an object): its flags, dimensions and name,
    /// then these bytes as one element.
    Opaque(u8, Vec<u8>),
    /// `[]`.
    Empty,
}

impl Value {
    /// A 1 × 1 double.
    pub fn scalar(x: f64) -> Value {
        Value::Double(vec![1, 1], vec![x])
    }

    /// A 1 × 1 struct.
    pub fn record(fields: Vec<(&str, Value)>) -> Value {
        let (names, values): (Vec<String>, Vec<Value>) =
            fields.into_iter().map(|(n, v)| (n.to_owned(), v)).unzip();
        Value::Struct(vec![1, 1], names, vec![values])
    }

    /// A 1 × n struct array with the same fields in every element.
    pub fn records(names: &[&str], elements: Vec<Vec<Value>>) -> Value {
        Value::Struct(
            vec![1, elements.len()],
            names.iter().map(|n| (*n).to_owned()).collect(),
            elements,
        )
    }
}

/// The 128-byte header.
pub fn header(order: Order) -> Vec<u8> {
    let mut h = b"MATLAB 5.0 MAT-file, Platform: GLNXA64, Created on: Tue Jan  2 03:04:05 2024, \
                  written by kymo-testgen"
        .to_vec();
    h.resize(116, b' ');
    h.extend_from_slice(&[0; 8]);
    let mut w = Writer::new(order);
    w.u16(0x0100);
    // The endian indicator: "IM" read as a u16 in the file's order.
    w.u16(u16::from_le_bytes(*b"IM"));
    h.extend_from_slice(&w.out);
    h
}

/// A whole file: the header and each variable.
pub fn file(variables: &[(&str, Value)], order: Order) -> Vec<u8> {
    let mut bytes = header(order);
    for (name, value) in variables {
        bytes.extend_from_slice(&variable(name, value, order));
    }
    bytes
}

/// A top-level `miMATRIX` element.
pub fn variable(name: &str, value: &Value, order: Order) -> Vec<u8> {
    let mut w = Writer::new(order);
    w.matrix(name, value);
    w.out
}

/// A tag and its data, padded, for elements written by hand (a compressed element, say).
pub fn element(kind: u32, data: &[u8], order: Order) -> Vec<u8> {
    let mut w = Writer::new(order);
    w.element(kind, data);
    w.out
}

struct Writer {
    order: Order,
    out: Vec<u8>,
}

impl Writer {
    fn new(order: Order) -> Self {
        Writer {
            order,
            out: Vec::new(),
        }
    }

    fn u16(&mut self, x: u16) {
        let b = match self.order {
            Order::Little => x.to_le_bytes(),
            Order::Big => x.to_be_bytes(),
        };
        self.out.extend_from_slice(&b);
    }

    fn u32(&mut self, x: u32) {
        let b = match self.order {
            Order::Little => x.to_le_bytes(),
            Order::Big => x.to_be_bytes(),
        };
        self.out.extend_from_slice(&b);
    }

    /// A data element: packed into its tag if it has 1 to 4 bytes, else tag, data and
    /// padding to 8 bytes.
    fn element(&mut self, kind: u32, data: &[u8]) {
        if (1..=4).contains(&data.len()) {
            self.u32(((data.len() as u32) << 16) | kind);
            self.out.extend_from_slice(data);
            self.out.resize(self.out.len() + 4 - data.len(), 0);
            return;
        }
        self.u32(kind);
        self.u32(data.len() as u32);
        self.out.extend_from_slice(data);
        self.out.resize(self.out.len().div_ceil(8) * 8, 0);
    }

    fn numbers<const N: usize>(&self, values: impl Iterator<Item = ([u8; N], [u8; N])>) -> Vec<u8> {
        values
            .flat_map(|(le, be)| match self.order {
                Order::Little => le,
                Order::Big => be,
            })
            .collect()
    }

    fn matrix(&mut self, name: &str, value: &Value) {
        let mut inner = Writer::new(self.order);
        inner.array(name, value);
        self.u32(MI_MATRIX);
        self.u32(inner.out.len() as u32);
        self.out.extend_from_slice(&inner.out);
    }

    /// Array flags, dimensions and name.
    fn array_header(&mut self, class: u8, flags: u32, dims: &[usize], name: &str) {
        let mut f = Writer::new(self.order);
        f.u32(u32::from(class) | flags);
        f.u32(0);
        self.element(MI_UINT32, &f.out);
        let mut d = Writer::new(self.order);
        for &x in dims {
            d.u32(x as u32);
        }
        self.element(MI_INT32, &d.out);
        self.element(MI_INT8, name.as_bytes());
    }

    fn array(&mut self, name: &str, value: &Value) {
        match value {
            Value::Double(dims, values) => {
                self.array_header(MX_DOUBLE, 0, dims, name);
                let (kind, bytes) = self.compact(values);
                self.element(kind, &bytes);
            }
            Value::Single(dims, values) => {
                self.array_header(MX_SINGLE, 0, dims, name);
                let bytes = self.numbers(values.iter().map(|v| (v.to_le_bytes(), v.to_be_bytes())));
                self.element(MI_SINGLE, &bytes);
            }
            Value::Raw {
                class,
                storage,
                dims,
                bytes,
            } => {
                self.array_header(*class, 0, dims, name);
                self.element(*storage, bytes);
            }
            Value::Char(text) => {
                let units: Vec<u16> = text.encode_utf16().collect();
                self.array_header(MX_CHAR, 0, &[1, units.len()], name);
                let bytes = self.numbers(units.iter().map(|u| (u.to_le_bytes(), u.to_be_bytes())));
                self.element(MI_UINT16, &bytes);
            }
            Value::Rows(rows) => {
                let rows: Vec<Vec<u16>> = rows.iter().map(|r| r.encode_utf16().collect()).collect();
                let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                self.array_header(MX_CHAR, 0, &[rows.len(), width], name);
                let units = (0..width).flat_map(|c| {
                    rows.iter()
                        .map(move |r| r.get(c).copied().unwrap_or(u16::from(b' ')))
                });
                let bytes = self.numbers(units.map(|u| (u.to_le_bytes(), u.to_be_bytes())));
                self.element(MI_UINT16, &bytes);
            }
            Value::Struct(dims, names, elements) => {
                self.array_header(MX_STRUCT, 0, dims, name);
                let mut len = Writer::new(self.order);
                len.u32(32);
                self.element(MI_INT32, &len.out);
                let mut field_names = Vec::new();
                for n in names {
                    let mut b = n.as_bytes().to_vec();
                    b.resize(32, 0);
                    field_names.extend_from_slice(&b);
                }
                self.element(MI_INT8, &field_names);
                for element in elements {
                    for field in element {
                        self.matrix("", field);
                    }
                }
            }
            Value::Cell(dims, values) => {
                self.array_header(MX_CELL, 0, dims, name);
                for v in values {
                    self.matrix("", v);
                }
            }
            Value::Logical(values) => {
                self.array_header(MX_UINT8, 0x0200, &[1, values.len()], name);
                let bytes: Vec<u8> = values.iter().map(|&b| u8::from(b)).collect();
                self.element(MI_UINT8, &bytes);
            }
            Value::Opaque(class, bytes) => {
                self.array_header(*class, 0, &[1, 1], name);
                self.element(MI_UINT8, bytes);
            }
            Value::Empty => {
                self.array_header(MX_DOUBLE, 0, &[0, 0], name);
                self.element(MI_DOUBLE, &[]);
            }
        }
    }

    /// Doubles in the smallest type that holds them all: an integer type if they are whole
    /// numbers in its range, double otherwise.
    fn compact(&self, values: &[f64]) -> (u32, Vec<u8>) {
        let whole = values
            .iter()
            .all(|v| v.is_finite() && v.fract() == 0.0 && !(*v == 0.0 && v.is_sign_negative()));
        let within = |lo: f64, hi: f64| whole && values.iter().all(|v| (lo..=hi).contains(v));
        if !values.is_empty() {
            if within(0.0, 255.0) {
                return (MI_UINT8, values.iter().map(|&v| v as u8).collect());
            }
            if within(-128.0, 127.0) {
                return (MI_INT8, values.iter().map(|&v| v as i8 as u8).collect());
            }
            if within(0.0, 65_535.0) {
                let v = values.iter().map(|&v| v as u16);
                return (
                    MI_UINT16,
                    self.numbers(v.map(|v| (v.to_le_bytes(), v.to_be_bytes()))),
                );
            }
            if within(-32_768.0, 32_767.0) {
                let v = values.iter().map(|&v| v as i16);
                return (
                    MI_INT16,
                    self.numbers(v.map(|v| (v.to_le_bytes(), v.to_be_bytes()))),
                );
            }
            if within(0.0, 4_294_967_295.0) {
                let v = values.iter().map(|&v| v as u32);
                return (
                    MI_UINT32,
                    self.numbers(v.map(|v| (v.to_le_bytes(), v.to_be_bytes()))),
                );
            }
            if within(-2_147_483_648.0, 2_147_483_647.0) {
                let v = values.iter().map(|&v| v as i32);
                return (
                    MI_INT32,
                    self.numbers(v.map(|v| (v.to_le_bytes(), v.to_be_bytes()))),
                );
            }
        }
        let bytes = self.numbers(values.iter().map(|v| (v.to_le_bytes(), v.to_be_bytes())));
        (MI_DOUBLE, bytes)
    }
}
