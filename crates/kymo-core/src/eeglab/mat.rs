//! MATLAB's level 5 MAT-files (what MATLAB's `-v6` and `-v7` saves write) over a mapped
//! file, read lazily: a variable, a struct field or a cell is parsed only when asked for,
//! and numeric arrays stay ranges of the file, so large arrays are never copied.
//!
//! A file is a 128-byte header (text, then the version and an endian indicator, "IM" for
//! little-endian files) and a sequence of data elements, each a tag (type and byte count)
//! and its data, padded to 8 bytes. Elements of up to 4 bytes may be packed into their tag.
//! A variable is a `miMATRIX` element: array flags (the class), dimensions, the name, then
//! the class's parts (the real and imaginary data, a char array's characters, a struct's
//! field names and its fields as nested `miMATRIX` elements, a cell's elements).

use std::ops::Range;

use super::EeglabError;
use crate::decode::{ByteOrder, Decoder, Encoding};

/// Data element types.
const MI_INT8: u32 = 1;
const MI_UINT8: u32 = 2;
const MI_INT16: u32 = 3;
const MI_UINT16: u32 = 4;
const MI_INT32: u32 = 5;
const MI_UINT32: u32 = 6;
const MI_SINGLE: u32 = 7;
const MI_DOUBLE: u32 = 9;
const MI_INT64: u32 = 12;
const MI_UINT64: u32 = 13;
const MI_MATRIX: u32 = 14;
const MI_COMPRESSED: u32 = 15;
const MI_UTF8: u32 = 16;
const MI_UTF16: u32 = 17;
const MI_UTF32: u32 = 18;

/// Array classes (the low byte of the array flags).
const MX_CELL: u32 = 1;
const MX_STRUCT: u32 = 2;
const MX_CHAR: u32 = 4;
const MX_DOUBLE: u32 = 6;
const MX_UINT64: u32 = 15;
/// The array-flags bit of complex arrays.
const COMPLEX: u32 = 0x0800;

fn malformed(message: impl Into<String>) -> EeglabError {
    EeglabError::Mat(message.into())
}

/// The variables of a MAT-file, by name, unparsed.
#[derive(Debug)]
pub(super) struct Variables<'a> {
    pub(super) list: Vec<(String, Element<'a>)>,
    /// Compressed variables (`miCOMPRESSED`), which are not read.
    pub(super) compressed: usize,
}

impl<'a> Variables<'a> {
    /// Reads the header and the variables' names. `bytes` is the whole file.
    pub(super) fn read(bytes: &'a [u8]) -> Result<Variables<'a>, EeglabError> {
        if bytes.len() < 128 {
            return Err(malformed("the file is shorter than a MAT-file header"));
        }
        let order = match &bytes[126..128] {
            b"IM" => ByteOrder::Little,
            b"MI" => ByteOrder::Big,
            other => {
                return Err(malformed(format!(
                    "unknown endian indicator {:?}",
                    String::from_utf8_lossy(other)
                )));
            }
        };
        let file = File { bytes, order };
        let mut list = Vec::new();
        let mut compressed = 0;
        let mut at = 128;
        while at < bytes.len() {
            // Padding at the end of a file is not an element.
            if bytes.len() - at < 8 && bytes[at..].iter().all(|&b| b == 0) {
                break;
            }
            let tag = file.tag(at, bytes.len())?;
            match tag.kind {
                MI_MATRIX => {
                    let element = Element {
                        file,
                        start: tag.data.start,
                        end: tag.data.end,
                    };
                    let name = element.header()?.name;
                    list.push((name, element));
                }
                MI_COMPRESSED => compressed += 1,
                _ => {}
            }
            // Compressed elements are not padded.
            at = if tag.kind == MI_COMPRESSED {
                tag.data.end
            } else {
                tag.next
            };
        }
        Ok(Variables { list, compressed })
    }

    /// The variable `name`, if the file has one.
    pub(super) fn get(&self, name: &str) -> Option<Element<'a>> {
        self.list.iter().find(|(n, _)| n == name).map(|(_, e)| *e)
    }
}

#[derive(Clone, Copy, Debug)]
struct File<'a> {
    bytes: &'a [u8],
    order: ByteOrder,
}

/// A data element's tag.
#[derive(Debug)]
struct Tag {
    kind: u32,
    /// The element's data, within the file.
    data: Range<usize>,
    /// Where the next element starts (after padding), at most `end`.
    next: usize,
}

impl<'a> File<'a> {
    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.bytes.get(at..at + 4)?.try_into().ok()?;
        Some(match self.order {
            ByteOrder::Little => u32::from_le_bytes(b),
            ByteOrder::Big => u32::from_be_bytes(b),
        })
    }

    /// The tag of the element at `at`, which must end by `end`.
    fn tag(&self, at: usize, end: usize) -> Result<Tag, EeglabError> {
        let truncated = || malformed(format!("the element at byte {at} is truncated"));
        if at.checked_add(8).is_none_or(|e| e > end) {
            return Err(truncated());
        }
        let first = self.u32(at).ok_or_else(truncated)?;
        if first >> 16 != 0 {
            // Packed: the byte count in the upper half, the data in the tag's second word.
            let n = (first >> 16) as usize;
            if n > 4 {
                return Err(malformed(format!(
                    "the packed element at byte {at} claims {n} bytes"
                )));
            }
            return Ok(Tag {
                kind: first & 0xffff,
                data: at + 4..at + 4 + n,
                next: at + 8,
            });
        }
        let n = self.u32(at + 4).ok_or_else(truncated)? as usize;
        let start = at + 8;
        let data_end = start
            .checked_add(n)
            .filter(|&e| e <= end)
            .ok_or_else(truncated)?;
        let next = data_end.div_ceil(8).saturating_mul(8).min(end);
        Ok(Tag {
            kind: first,
            data: start..data_end,
            next,
        })
    }

    /// The elements in `range`, in order.
    fn tags(&self, range: Range<usize>) -> Tags<'a> {
        Tags {
            file: *self,
            at: range.start,
            end: range.end,
        }
    }
}

struct Tags<'a> {
    file: File<'a>,
    at: usize,
    end: usize,
}

impl Tags<'_> {
    /// The next element, which must exist.
    fn next(&mut self, what: &str) -> Result<Tag, EeglabError> {
        if self.at >= self.end {
            return Err(malformed(format!("an array ends before its {what}")));
        }
        let tag = self.file.tag(self.at, self.end)?;
        self.at = tag.next;
        Ok(tag)
    }
}

/// An unparsed `miMATRIX` element: a variable, a struct field or a cell's element.
#[derive(Clone, Copy, Debug)]
pub(super) struct Element<'a> {
    file: File<'a>,
    /// The element's data (its subelements), within the file: `start..end`.
    start: usize,
    end: usize,
}

/// What every array starts with.
struct ArrayHeader {
    class: u32,
    complex: bool,
    dims: Vec<usize>,
    name: String,
    /// Where the class's parts start.
    rest: usize,
}

impl<'a> Element<'a> {
    fn header(&self) -> Result<ArrayHeader, EeglabError> {
        let mut tags = self.file.tags(self.start..self.end);
        let flags = tags.next("array flags")?;
        let flags = self
            .file
            .u32(flags.data.start)
            .filter(|_| flags.data.len() >= 4)
            .ok_or_else(|| malformed("array flags shorter than 4 bytes"))?;
        let dims = tags.next("dimensions")?;
        if dims.kind != MI_INT32 || !dims.data.len().is_multiple_of(4) {
            return Err(malformed("array dimensions are not int32"));
        }
        let dims = dims
            .data
            .clone()
            .step_by(4)
            .map(|at| {
                let d = self.file.u32(at).unwrap_or(0) as i32;
                usize::try_from(d).map_err(|_| malformed(format!("negative dimension {d}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let name = tags.next("name")?;
        let name = String::from_utf8_lossy(&self.file.bytes[name.data]).into_owned();
        Ok(ArrayHeader {
            class: flags & 0xff,
            complex: flags & COMPLEX != 0,
            dims,
            name,
            rest: tags.at,
        })
    }

    /// Parses the array.
    pub(super) fn value(&self) -> Result<Value<'a>, EeglabError> {
        // An element without data is an empty array, as MATLAB writes `[]` in cells.
        if self.start >= self.end {
            return Ok(Value::Numeric(Numeric::empty(self.file)));
        }
        let header = self.header()?;
        let count = header
            .dims
            .iter()
            .try_fold(1usize, |n, &d| n.checked_mul(d))
            .ok_or_else(|| malformed(format!("dimensions {:?} overflow", header.dims)))?;
        let mut tags = self.file.tags(header.rest..self.end);
        Ok(match header.class {
            MX_DOUBLE..=MX_UINT64 => {
                let real = tags.next("data")?;
                let encoding = storage(real.kind)?;
                let size = encoding.size();
                if real.data.len() != count.saturating_mul(size) {
                    return Err(malformed(format!(
                        "an array of {count} elements holds {} bytes of {size}-byte values",
                        real.data.len()
                    )));
                }
                Value::Numeric(Numeric {
                    file: self.file,
                    dims: header.dims,
                    encoding,
                    data: real.data,
                    complex: header.complex,
                })
            }
            MX_CHAR => {
                let data = tags.next("characters")?;
                Value::Char(self.chars(data, &header.dims, count)?)
            }
            MX_STRUCT => {
                let length = tags.next("field name length")?;
                let length = self
                    .file
                    .u32(length.data.start)
                    .filter(|&l| l > 0 && length.data.len() >= 4)
                    .ok_or_else(|| malformed("invalid field name length"))?
                    as usize;
                let names = tags.next("field names")?;
                let names: Vec<String> = self.file.bytes[names.data]
                    .chunks(length)
                    .map(|name| {
                        let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                        String::from_utf8_lossy(&name[..end]).into_owned()
                    })
                    .collect();
                let fields = self.elements(&mut tags, count.saturating_mul(names.len()))?;
                Value::Struct(Struct {
                    dims: header.dims,
                    names,
                    fields,
                })
            }
            MX_CELL => Value::Cell(self.elements(&mut tags, count)?),
            _ => Value::Other,
        })
    }

    /// The next `count` subelements, each an array.
    fn elements(&self, tags: &mut Tags<'a>, count: usize) -> Result<Vec<Element<'a>>, EeglabError> {
        // Each takes at least a tag's 8 bytes, which bounds what a broken count allocates.
        if count > (tags.end - tags.at) / 8 {
            return Err(malformed(format!(
                "{count} array elements don't fit into {} bytes",
                tags.end - tags.at
            )));
        }
        (0..count)
            .map(|_| {
                let tag = tags.next("elements")?;
                if tag.kind != MI_MATRIX {
                    return Err(malformed(format!(
                        "an element of type {} where an array was expected",
                        tag.kind
                    )));
                }
                Ok(Element {
                    file: self.file,
                    start: tag.data.start,
                    end: tag.data.end,
                })
            })
            .collect()
    }

    /// The text of a char array of `count` characters. A matrix of several rows (stored
    /// column by column) becomes its rows, each without trailing blanks, one per line.
    fn chars(&self, data: Tag, dims: &[usize], count: usize) -> Result<String, EeglabError> {
        let bytes = &self.file.bytes[data.data];
        let little = self.file.order == ByteOrder::Little;
        let units = |n: usize| {
            bytes.chunks_exact(n).map(move |c| {
                let mut a = [0u8; 4];
                a[..n].copy_from_slice(c);
                if little {
                    u32::from_le_bytes(a)
                } else {
                    let mut b = [0u8; 4];
                    b[4 - n..].copy_from_slice(c);
                    u32::from_be_bytes(b)
                }
            })
        };
        let chars: Vec<char> = match data.kind {
            MI_UINT16 | MI_UTF16 => char::decode_utf16(units(2).map(|u| u as u16))
                .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect(),
            MI_UTF8 => String::from_utf8_lossy(bytes).chars().collect(),
            MI_UTF32 => units(4)
                .map(|u| char::from_u32(u).unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect(),
            // Latin-1, whose code points are its bytes.
            MI_INT8 | MI_UINT8 => bytes.iter().map(|&b| char::from(b)).collect(),
            other => {
                return Err(malformed(format!("characters stored as type {other}")));
            }
        };
        let rows = dims.first().copied().unwrap_or(1);
        if rows <= 1 || chars.len() != count {
            return Ok(chars.into_iter().collect());
        }
        let columns = count / rows;
        let lines: Vec<String> = (0..rows)
            .map(|r| {
                let line: String = (0..columns).map(|c| chars[c * rows + r]).collect();
                line.trim_end().to_owned()
            })
            .collect();
        Ok(lines.join("\n"))
    }
}

/// The encoding of numeric data stored as element type `kind`.
fn storage(kind: u32) -> Result<Encoding, EeglabError> {
    Ok(match kind {
        MI_INT8 => Encoding::I8,
        MI_UINT8 => Encoding::U8,
        MI_INT16 => Encoding::I16,
        MI_UINT16 => Encoding::U16,
        MI_INT32 => Encoding::I32,
        MI_UINT32 => Encoding::U32,
        MI_SINGLE => Encoding::F32,
        MI_DOUBLE => Encoding::F64,
        MI_INT64 => Encoding::I64,
        MI_UINT64 => Encoding::U64,
        other => return Err(malformed(format!("numeric data stored as type {other}"))),
    })
}

/// A parsed array.
#[derive(Debug)]
pub(super) enum Value<'a> {
    Numeric(Numeric<'a>),
    Char(String),
    Struct(Struct<'a>),
    /// A cell array's elements, in MATLAB's (column-major) order.
    Cell(Vec<Element<'a>>),
    /// A class kymo doesn't read: objects, sparse arrays, function handles and others.
    Other,
}

impl Value<'_> {
    /// The text of a char array; `""` for an empty array of any class.
    pub(super) fn text(&self) -> Option<String> {
        match self {
            Value::Char(s) => Some(s.clone()),
            Value::Numeric(n) if n.len() == 0 => Some(String::new()),
            _ => None,
        }
    }

    /// The value of a numeric array of one element.
    pub(super) fn scalar(&self) -> Option<f64> {
        match self {
            Value::Numeric(n) if n.len() == 1 => n.get(0),
            _ => None,
        }
    }

    /// Whether this is an empty array (of any class, `[]`, `''` or `{}`).
    pub(super) fn is_empty(&self) -> bool {
        match self {
            Value::Numeric(n) => n.len() == 0,
            Value::Char(s) => s.is_empty(),
            Value::Cell(c) => c.is_empty(),
            Value::Struct(s) => s.len() == 0,
            Value::Other => false,
        }
    }
}

/// A numeric array, its data left in the file.
#[derive(Debug)]
pub(super) struct Numeric<'a> {
    file: File<'a>,
    pub(super) dims: Vec<usize>,
    /// How the values are stored, which may be a smaller type than the class: MATLAB stores
    /// doubles that fit as integers.
    pub(super) encoding: Encoding,
    /// The real part's bytes, within the file.
    pub(super) data: Range<usize>,
    pub(super) complex: bool,
}

impl<'a> Numeric<'a> {
    fn empty(file: File<'a>) -> Self {
        Numeric {
            file,
            dims: vec![0, 0],
            encoding: Encoding::F64,
            data: 0..0,
            complex: false,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.data.len() / self.encoding.size()
    }

    pub(super) fn order(&self) -> ByteOrder {
        self.file.order
    }

    /// Element `k`, in f64.
    pub(super) fn get(&self, k: usize) -> Option<f64> {
        let at = self.data.start + k.checked_mul(self.encoding.size())?;
        let bytes = self.file.bytes.get(at..self.data.end)?;
        Decoder::new(self.encoding, self.file.order).value(bytes)
    }
}

/// A struct array, its fields unparsed.
#[derive(Debug)]
pub(super) struct Struct<'a> {
    pub(super) dims: Vec<usize>,
    pub(super) names: Vec<String>,
    /// Element-major: the fields of element 0, then of element 1, …
    fields: Vec<Element<'a>>,
}

impl<'a> Struct<'a> {
    /// Number of elements.
    pub(super) fn len(&self) -> usize {
        if self.names.is_empty() {
            self.dims.iter().fold(1usize, |p, &d| p.saturating_mul(d))
        } else {
            self.fields.len() / self.names.len()
        }
    }

    /// Field `name` of element `k`, if the struct has that field.
    pub(super) fn field(&self, k: usize, name: &str) -> Result<Option<Value<'a>>, EeglabError> {
        let Some(f) = self.names.iter().position(|n| n == name) else {
            return Ok(None);
        };
        match self.fields.get(k * self.names.len() + f) {
            Some(element) => element.value().map(Some),
            None => Ok(None),
        }
    }
}
