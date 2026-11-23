use crate::error::{NbtError, Result};
use indexmap::IndexMap;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endianness {
    Big,
    Little,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Zlib,
}

pub mod tag_id {
    pub const END: u8 = 0;
    pub const BYTE: u8 = 1;
    pub const SHORT: u8 = 2;
    pub const INT: u8 = 3;
    pub const LONG: u8 = 4;
    pub const FLOAT: u8 = 5;
    pub const DOUBLE: u8 = 6;
    pub const BYTE_ARRAY: u8 = 7;
    pub const STRING: u8 = 8;
    pub const LIST: u8 = 9;
    pub const COMPOUND: u8 = 10;
    pub const INT_ARRAY: u8 = 11;
    pub const LONG_ARRAY: u8 = 12;
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NbtCompound {
    entries: IndexMap<String, NbtTag>,
}

impl NbtCompound {
    pub fn new() -> Self {
        Self {
            entries: IndexMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    pub fn get(&self, key: &str) -> Option<&NbtTag> {
        self.entries.get(key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut NbtTag> {
        self.entries.get_mut(key)
    }

    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<NbtTag>) -> Option<NbtTag> {
        self.entries.insert(key.into(), value.into())
    }

    pub fn remove(&mut self, key: &str) -> Option<NbtTag> {
        self.entries.shift_remove(key)
    }

    pub fn rename_key(&mut self, old_key: &str, new_key: impl Into<String>) -> bool {
        let new_key = new_key.into();
        if old_key == new_key {
            return self.contains_key(old_key);
        }
        if self.contains_key(&new_key) {
            return false;
        }
        if let Some(idx) = self.entries.get_index_of(old_key) {
            let Some((_, val)) = self.entries.shift_remove_index(idx) else {
                return false;
            };
            // Re-insert preserving order via rebuild
            let mut rebuilt = IndexMap::with_capacity(self.entries.len() + 1);
            let mut inserted = false;
            for (i, (k, v)) in self.entries.drain(..).enumerate() {
                if i == idx {
                    rebuilt.insert(new_key.clone(), val.clone());
                    inserted = true;
                }
                rebuilt.insert(k, v);
            }
            if !inserted {
                rebuilt.insert(new_key, val);
            }
            self.entries = rebuilt;
            true
        } else {
            false
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &NbtTag)> {
        self.entries.iter()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut NbtTag)> {
        self.entries.iter_mut()
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.keys()
    }

    pub fn get_byte(&self, key: &str) -> Option<i8> {
        match self.get(key)? {
            NbtTag::Byte(v) => Some(*v),
            _ => None,
        }
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get_byte(key).map(|v| v != 0)
    }

    pub fn get_short(&self, key: &str) -> Option<i16> {
        match self.get(key)? {
            NbtTag::Short(v) => Some(*v),
            _ => None,
        }
    }

    pub fn get_int(&self, key: &str) -> Option<i32> {
        match self.get(key)? {
            NbtTag::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn get_long(&self, key: &str) -> Option<i64> {
        match self.get(key)? {
            NbtTag::Long(v) => Some(*v),
            _ => None,
        }
    }

    pub fn get_float(&self, key: &str) -> Option<f32> {
        match self.get(key)? {
            NbtTag::Float(v) => Some(*v),
            _ => None,
        }
    }

    pub fn get_double(&self, key: &str) -> Option<f64> {
        match self.get(key)? {
            NbtTag::Double(v) => Some(*v),
            _ => None,
        }
    }

    pub fn get_string(&self, key: &str) -> Option<&str> {
        match self.get(key)? {
            NbtTag::String(v) => Some(v.as_str()),
            _ => None,
        }
    }

    pub fn get_compound(&self, key: &str) -> Option<&NbtCompound> {
        match self.get(key)? {
            NbtTag::Compound(v) => Some(v),
            _ => None,
        }
    }

    pub fn get_compound_mut(&mut self, key: &str) -> Option<&mut NbtCompound> {
        match self.get_mut(key)? {
            NbtTag::Compound(v) => Some(v),
            _ => None,
        }
    }

    pub fn get_list(&self, key: &str) -> Option<&NbtList> {
        match self.get(key)? {
            NbtTag::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn get_list_mut(&mut self, key: &str) -> Option<&mut NbtList> {
        match self.get_mut(key)? {
            NbtTag::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn get_byte_array(&self, key: &str) -> Option<&[i8]> {
        match self.get(key)? {
            NbtTag::ByteArray(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    pub fn get_int_array(&self, key: &str) -> Option<&[i32]> {
        match self.get(key)? {
            NbtTag::IntArray(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    pub fn get_long_array(&self, key: &str) -> Option<&[i64]> {
        match self.get(key)? {
            NbtTag::LongArray(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    pub fn get_number_as_f64(&self, key: &str) -> Option<f64> {
        match self.get(key)? {
            NbtTag::Byte(v) => Some(*v as f64),
            NbtTag::Short(v) => Some(*v as f64),
            NbtTag::Int(v) => Some(*v as f64),
            NbtTag::Long(v) => Some(*v as f64),
            NbtTag::Float(v) => Some(*v as f64),
            NbtTag::Double(v) => Some(*v),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NbtList {
    element_tag_id: u8,
    items: Vec<NbtTag>,
}

impl Default for NbtList {
    fn default() -> Self {
        Self::new_empty()
    }
}

impl NbtList {
    pub fn new_empty() -> Self {
        Self {
            element_tag_id: tag_id::END,
            items: Vec::new(),
        }
    }

    pub fn with_element_type(element_tag_id: u8) -> Self {
        Self {
            element_tag_id,
            items: Vec::new(),
        }
    }

    pub fn from_items(items: Vec<NbtTag>) -> Result<Self> {
        let Some(first) = items.first() else {
            return Ok(Self::new_empty());
        };
        let expected = first.tag_id();
        for item in &items {
            if item.tag_id() != expected {
                return Err(NbtError::ListTypeMismatch {
                    expected,
                    found: item.tag_id(),
                });
            }
        }
        Ok(Self {
            element_tag_id: expected,
            items,
        })
    }

    pub fn element_tag_id(&self) -> u8 {
        self.element_tag_id
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[NbtTag] {
        &self.items
    }

    pub fn items_mut(&mut self) -> &mut Vec<NbtTag> {
        &mut self.items
    }

    pub fn push(&mut self, tag: NbtTag) -> Result<()> {
        let tid = tag.tag_id();
        if self.items.is_empty() && self.element_tag_id == tag_id::END {
            self.element_tag_id = tid;
        } else if tid != self.element_tag_id {
            return Err(NbtError::ListTypeMismatch {
                expected: self.element_tag_id,
                found: tid,
            });
        }
        self.items.push(tag);
        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> Option<NbtTag> {
        if index < self.items.len() {
            let item = self.items.remove(index);
            if self.items.is_empty() {
                self.element_tag_id = tag_id::END;
            }
            Some(item)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum NbtTag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<i8>),
    String(String),
    List(NbtList),
    Compound(NbtCompound),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl NbtTag {
    pub fn tag_id(&self) -> u8 {
        match self {
            NbtTag::Byte(_) => tag_id::BYTE,
            NbtTag::Short(_) => tag_id::SHORT,
            NbtTag::Int(_) => tag_id::INT,
            NbtTag::Long(_) => tag_id::LONG,
            NbtTag::Float(_) => tag_id::FLOAT,
            NbtTag::Double(_) => tag_id::DOUBLE,
            NbtTag::ByteArray(_) => tag_id::BYTE_ARRAY,
            NbtTag::String(_) => tag_id::STRING,
            NbtTag::List(_) => tag_id::LIST,
            NbtTag::Compound(_) => tag_id::COMPOUND,
            NbtTag::IntArray(_) => tag_id::INT_ARRAY,
            NbtTag::LongArray(_) => tag_id::LONG_ARRAY,
        }
    }

    pub fn type_name(&self) -> &'static str {
        tag_type_name(self.tag_id())
    }

    pub fn default_for_tag_id(id: u8) -> Option<Self> {
        match id {
            tag_id::BYTE => Some(NbtTag::Byte(0)),
            tag_id::SHORT => Some(NbtTag::Short(0)),
            tag_id::INT => Some(NbtTag::Int(0)),
            tag_id::LONG => Some(NbtTag::Long(0)),
            tag_id::FLOAT => Some(NbtTag::Float(0.0)),
            tag_id::DOUBLE => Some(NbtTag::Double(0.0)),
            tag_id::BYTE_ARRAY => Some(NbtTag::ByteArray(Vec::new())),
            tag_id::STRING => Some(NbtTag::String(String::new())),
            tag_id::LIST => Some(NbtTag::List(NbtList::new_empty())),
            tag_id::COMPOUND => Some(NbtTag::Compound(NbtCompound::new())),
            tag_id::INT_ARRAY => Some(NbtTag::IntArray(Vec::new())),
            tag_id::LONG_ARRAY => Some(NbtTag::LongArray(Vec::new())),
            _ => None,
        }
    }
}

pub fn tag_type_name(id: u8) -> &'static str {
    match id {
        tag_id::END => "TAG_End",
        tag_id::BYTE => "TAG_Byte",
        tag_id::SHORT => "TAG_Short",
        tag_id::INT => "TAG_Int",
        tag_id::LONG => "TAG_Long",
        tag_id::FLOAT => "TAG_Float",
        tag_id::DOUBLE => "TAG_Double",
        tag_id::BYTE_ARRAY => "TAG_Byte_Array",
        tag_id::STRING => "TAG_String",
        tag_id::LIST => "TAG_List",
        tag_id::COMPOUND => "TAG_Compound",
        tag_id::INT_ARRAY => "TAG_Int_Array",
        tag_id::LONG_ARRAY => "TAG_Long_Array",
        _ => "TAG_Unknown",
    }
}

impl fmt::Display for NbtTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NbtTag::Byte(v) => write!(f, "{v}b"),
            NbtTag::Short(v) => write!(f, "{v}s"),
            NbtTag::Int(v) => write!(f, "{v}"),
            NbtTag::Long(v) => write!(f, "{v}L"),
            NbtTag::Float(v) => write!(f, "{v}f"),
            NbtTag::Double(v) => write!(f, "{v}d"),
            NbtTag::ByteArray(v) => write!(f, "[B; {} bytes]", v.len()),
            NbtTag::String(v) => write!(f, "\"{v}\""),
            NbtTag::List(v) => write!(f, "[{} items]", v.len()),
            NbtTag::Compound(v) => write!(f, "{{{} entries}}", v.len()),
            NbtTag::IntArray(v) => write!(f, "[I; {} ints]", v.len()),
            NbtTag::LongArray(v) => write!(f, "[L; {} longs]", v.len()),
        }
    }
}

impl From<i8> for NbtTag {
    fn from(v: i8) -> Self {
        NbtTag::Byte(v)
    }
}
impl From<bool> for NbtTag {
    fn from(v: bool) -> Self {
        NbtTag::Byte(if v { 1 } else { 0 })
    }
}
impl From<i16> for NbtTag {
    fn from(v: i16) -> Self {
        NbtTag::Short(v)
    }
}
impl From<i32> for NbtTag {
    fn from(v: i32) -> Self {
        NbtTag::Int(v)
    }
}
impl From<i64> for NbtTag {
    fn from(v: i64) -> Self {
        NbtTag::Long(v)
    }
}
impl From<f32> for NbtTag {
    fn from(v: f32) -> Self {
        NbtTag::Float(v)
    }
}
impl From<f64> for NbtTag {
    fn from(v: f64) -> Self {
        NbtTag::Double(v)
    }
}
impl From<String> for NbtTag {
    fn from(v: String) -> Self {
        NbtTag::String(v)
    }
}
impl From<&str> for NbtTag {
    fn from(v: &str) -> Self {
        NbtTag::String(v.to_string())
    }
}
impl From<NbtList> for NbtTag {
    fn from(v: NbtList) -> Self {
        NbtTag::List(v)
    }
}
impl From<NbtCompound> for NbtTag {
    fn from(v: NbtCompound) -> Self {
        NbtTag::Compound(v)
    }
}
