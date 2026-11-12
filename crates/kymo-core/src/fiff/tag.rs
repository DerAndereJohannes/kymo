//! FIFF's tags and blocks. Every tag has a 16-byte header (kind, type, size, next; signed
//! and unsigned 32-bit integers, big-endian) and its data. Block start and end tags, whose
//! data is the block's kind, nest the others into a tree.
//!
//! The tags are walked from the start of the file, by size, or by `next` where it is set
//! (forward or back), rather than through the file's directory: one path covers files
//! without a directory, with a stale one, and acquisitions cut short.

/// Tag kinds.
pub(super) mod kind {
    pub const FILE_ID: i32 = 100;
    pub const BLOCK_ID: i32 = 103;
    pub const PARENT_BLOCK_ID: i32 = 110;
    pub const BLOCK_START: i32 = 104;
    pub const BLOCK_END: i32 = 105;
    pub const NCHAN: i32 = 200;
    pub const SFREQ: i32 = 201;
    pub const CH_INFO: i32 = 203;
    pub const MEAS_DATE: i32 = 204;
    pub const DESCRIPTION: i32 = 206;
    pub const FIRST_SAMPLE: i32 = 208;
    pub const EXPERIMENTER: i32 = 212;
    pub const LOWPASS: i32 = 219;
    pub const HIGHPASS: i32 = 223;
    pub const LINE_FREQ: i32 = 235;
    pub const UTC_OFFSET: i32 = 159;
    pub const CH_KIND: i32 = 252;
    pub const CH_RANGE: i32 = 253;
    pub const CH_CAL: i32 = 254;
    pub const CH_UNIT: i32 = 256;
    pub const CH_UNIT_MUL: i32 = 257;
    pub const CH_DACQ_NAME: i32 = 258;
    pub const CH_COIL_TYPE: i32 = 350;
    pub const DATA_BUFFER: i32 = 300;
    pub const DATA_SKIP: i32 = 301;
    pub const PROJ_NAME: i32 = 501;
}

/// Block kinds.
pub(super) mod block {
    pub const MEAS: i32 = 100;
    pub const MEAS_INFO: i32 = 101;
    pub const RAW_DATA: i32 = 102;
    pub const EVOKED: i32 = 104;
    pub const CONTINUOUS_DATA: i32 = 112;
    pub const CH_INFO: i32 = 113;
    pub const IAS_RAW_DATA: i32 = 119;
    pub const MNE_EPOCHS: i32 = 373;
}

/// Data types.
pub(super) mod types {
    pub const SHORT: u32 = 2;
    pub const INT: u32 = 3;
    pub const FLOAT: u32 = 4;
    pub const DOUBLE: u32 = 5;
    pub const DAU_PACK16: u32 = 16;
    pub const COMPLEX_FLOAT: u32 = 20;
    pub const COMPLEX_DOUBLE: u32 = 21;
    pub const ID_STRUCT: u32 = 31;
}

/// One tag: its kind and type, and where its data is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Tag {
    pub kind: i32,
    pub typ: u32,
    /// Byte position of the tag's header.
    pub at: usize,
    /// Byte position of its data.
    pub data: usize,
    /// Bytes of data present: the tag's size, or less for a tag cut off by the end of the
    /// file.
    pub size: usize,
    /// Whether the end of the file cut the tag off.
    pub truncated: bool,
}

impl Tag {
    pub fn bytes<'a>(&self, map: &'a [u8]) -> &'a [u8] {
        map.get(self.data..self.data + self.size)
            .unwrap_or_default()
    }

    /// The first 32-bit integer.
    pub fn int(&self, map: &[u8]) -> Option<i32> {
        self.ints(map).next()
    }

    /// The 32-bit integers.
    pub fn ints<'a>(&self, map: &'a [u8]) -> impl Iterator<Item = i32> + 'a {
        self.bytes(map)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| i32::from_be_bytes(b))
    }

    /// A number: float, double or integer data.
    pub fn number(&self, map: &[u8]) -> Option<f64> {
        let b = self.bytes(map);
        match self.typ {
            types::FLOAT => Some(f64::from(f32::from_be_bytes(b.get(..4)?.try_into().ok()?))),
            types::DOUBLE => Some(f64::from_be_bytes(b.get(..8)?.try_into().ok()?)),
            types::SHORT => Some(f64::from(i16::from_be_bytes(b.get(..2)?.try_into().ok()?))),
            _ => self.int(map).map(f64::from),
        }
    }

    /// The time of an id struct (version, machine id, seconds, microseconds): seconds and
    /// microseconds since 1970.
    pub fn id_time(&self, map: &[u8]) -> Option<(i64, i64)> {
        if self.typ != types::ID_STRUCT {
            return None;
        }
        let mut v = self.ints(map).skip(3).map(i64::from);
        Some((v.next()?, v.next()?))
    }

    /// Text data: UTF-8 (invalid bytes replaced), up to its first NUL, trimmed.
    pub fn text(&self, map: &[u8]) -> String {
        let b = self.bytes(map);
        let b = b.split(|&x| x == 0).next().unwrap_or_default();
        String::from_utf8_lossy(b).trim().to_owned()
    }
}

/// A block: its kind, its own tags in file order, and the blocks inside it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Node {
    pub block: i32,
    pub tags: Vec<Tag>,
    pub children: Vec<Node>,
}

impl Node {
    /// The first block of kind `block` inside this one, depth first.
    pub fn find(&self, block: i32) -> Option<&Node> {
        self.children.iter().find_map(|c| {
            if c.block == block {
                Some(c)
            } else {
                c.find(block)
            }
        })
    }

    /// Every block of kind `block` inside this one, depth first.
    pub fn find_all<'a>(&'a self, block: i32, found: &mut Vec<&'a Node>) {
        for c in &self.children {
            if c.block == block {
                found.push(c);
            }
            c.find_all(block, found);
        }
    }

    /// This block's first tag of `kind`.
    pub fn tag(&self, kind: i32) -> Option<&Tag> {
        self.tags.iter().find(|t| t.kind == kind)
    }
}

/// Whether `map` starts with FIFF's file id tag.
pub(super) fn is_fiff(map: &[u8]) -> bool {
    let header = |k: usize| {
        map.get(4 * k..4 * k + 4)
            .map(|b| b.try_into().unwrap_or([0; 4]))
    };
    header(0) == Some(kind::FILE_ID.to_be_bytes())
        && header(1) == Some(types::ID_STRUCT.to_be_bytes())
}

/// The tree of the file's blocks; the root (block 0) holds the tags outside every block.
///
/// A file cut off inside a tag ends the walk there, with a warning; a cut-off data buffer
/// is kept (marked [`truncated`](Tag::truncated)) for the reader to use its whole frames,
/// other cut-off tags are dropped. Blocks left open at the end are closed.
pub(super) fn tree(map: &[u8], warnings: &mut Vec<String>) -> Node {
    let len = map.len();
    let mut stack = vec![Node::default()];
    let mut at = 0usize;
    // Targets of `next` pointers followed so far: a second jump to one is a loop.
    let mut jumped = std::collections::HashSet::new();
    while at < len {
        let Some(header) = map.get(at..at + 16) else {
            warnings.push(format!(
                "the file ends inside a tag header at byte {at}; the {} bytes there are \
                 ignored",
                len - at
            ));
            break;
        };
        let word = |k: usize| {
            [
                header[4 * k],
                header[4 * k + 1],
                header[4 * k + 2],
                header[4 * k + 3],
            ]
        };
        let kind = i32::from_be_bytes(word(0));
        let typ = u32::from_be_bytes(word(1));
        let size = i32::from_be_bytes(word(2));
        let next = i32::from_be_bytes(word(3));
        let Ok(size) = usize::try_from(size) else {
            warnings.push(format!(
                "the tag at byte {at} has a negative size ({size}); the file is read up to it"
            ));
            break;
        };
        let data = at + 16;
        let end = data.saturating_add(size);
        let truncated = end > len;
        let tag = Tag {
            kind,
            typ,
            at,
            data,
            size: end.min(len) - data,
            truncated,
        };
        match kind {
            _ if truncated && kind != kind::DATA_BUFFER => {
                warnings.push(format!(
                    "the file ends inside a tag (kind {kind}) at byte {at}, which is ignored"
                ));
                break;
            }
            kind::BLOCK_START => stack.push(Node {
                block: tag.int(map).unwrap_or(0),
                ..Node::default()
            }),
            kind::BLOCK_END => {
                if stack.len() > 1
                    && let Some(node) = stack.pop()
                    && let Some(parent) = stack.last_mut()
                {
                    parent.children.push(node);
                }
            }
            _ => {
                if let Some(node) = stack.last_mut() {
                    node.tags.push(tag);
                }
            }
        }
        if truncated {
            break;
        }
        at = match next {
            0 => end,
            -1 => break,
            // Files edited in place (bad channels added by MNE's tools) jump to tags appended
            // at the end and back.
            n if n > 0 && jumped.insert(n) => n as usize,
            n => {
                warnings.push(format!(
                    "the tag at byte {at} points to byte {n}, which is not a tag; the file is \
                     read up to it"
                ));
                break;
            }
        };
    }
    while stack.len() > 1 {
        if let Some(node) = stack.pop()
            && let Some(parent) = stack.last_mut()
        {
            parent.children.push(node);
        }
    }
    stack.pop().unwrap_or_default()
}
