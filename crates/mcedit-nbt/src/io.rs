use crate::error::{NbtError, Result};
use crate::value::{Compression, Endianness, NbtCompound, NbtList, NbtTag, tag_id};
use flate2::Compression as FlateCompression;
use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::{GzEncoder, ZlibEncoder};
use std::borrow::Cow;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::Path;

struct NbtReader<'a> {
    data: &'a [u8],
    pos: usize,
    endian: Endianness,
}

impl<'a> NbtReader<'a> {
    fn new(data: &'a [u8], endian: Endianness) -> Self {
        Self {
            data,
            pos: 0,
            endian,
        }
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        if self.pos + len > self.data.len() {
            return Err(NbtError::UnexpectedEof);
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn read_u8(&mut self) -> Result<u8> {
        Ok(self.read_bytes(1)?[0])
    }

    fn read_i8(&mut self) -> Result<i8> {
        Ok(self.read_u8()? as i8)
    }

    fn read_u16(&mut self) -> Result<u16> {
        let b = self.read_bytes(2)?;
        let arr = [b[0], b[1]];
        Ok(match self.endian {
            Endianness::Big => u16::from_be_bytes(arr),
            Endianness::Little => u16::from_le_bytes(arr),
        })
    }

    fn read_i16(&mut self) -> Result<i16> {
        Ok(self.read_u16()? as i16)
    }

    fn read_i32(&mut self) -> Result<i32> {
        let b = self.read_bytes(4)?;
        let arr = [b[0], b[1], b[2], b[3]];
        Ok(match self.endian {
            Endianness::Big => i32::from_be_bytes(arr),
            Endianness::Little => i32::from_le_bytes(arr),
        })
    }

    fn read_i64(&mut self) -> Result<i64> {
        let b = self.read_bytes(8)?;
        let arr = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        Ok(match self.endian {
            Endianness::Big => i64::from_be_bytes(arr),
            Endianness::Little => i64::from_le_bytes(arr),
        })
    }

    fn read_f32(&mut self) -> Result<f32> {
        let b = self.read_bytes(4)?;
        let arr = [b[0], b[1], b[2], b[3]];
        Ok(match self.endian {
            Endianness::Big => f32::from_be_bytes(arr),
            Endianness::Little => f32::from_le_bytes(arr),
        })
    }

    fn read_f64(&mut self) -> Result<f64> {
        let b = self.read_bytes(8)?;
        let arr = [b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]];
        Ok(match self.endian {
            Endianness::Big => f64::from_be_bytes(arr),
            Endianness::Little => f64::from_le_bytes(arr),
        })
    }

    fn read_string(&mut self) -> Result<String> {
        let len = self.read_u16()? as usize;
        let bytes = self.read_bytes(len)?;
        Ok(decode_mutf8(bytes))
    }

    fn read_payload(&mut self, tid: u8) -> Result<NbtTag> {
        match tid {
            tag_id::BYTE => Ok(NbtTag::Byte(self.read_i8()?)),
            tag_id::SHORT => Ok(NbtTag::Short(self.read_i16()?)),
            tag_id::INT => Ok(NbtTag::Int(self.read_i32()?)),
            tag_id::LONG => Ok(NbtTag::Long(self.read_i64()?)),
            tag_id::FLOAT => Ok(NbtTag::Float(self.read_f32()?)),
            tag_id::DOUBLE => Ok(NbtTag::Double(self.read_f64()?)),
            tag_id::BYTE_ARRAY => {
                let len = self.read_i32()?;
                if len < 0 {
                    return Err(NbtError::NegativeLength(len));
                }
                let raw = self.read_bytes(len as usize)?;
                Ok(NbtTag::ByteArray(raw.iter().map(|&b| b as i8).collect()))
            }
            tag_id::STRING => Ok(NbtTag::String(self.read_string()?)),
            tag_id::LIST => {
                let elem_tid = self.read_u8()?;
                let len = self.read_i32()?;
                if len < 0 {
                    return Err(NbtError::NegativeLength(len));
                }
                if len > 0 && (elem_tid == tag_id::END || elem_tid > tag_id::LONG_ARRAY) {
                    return Err(NbtError::InvalidTagId(elem_tid));
                }
                let mut list = NbtList::with_element_type(elem_tid);
                for _ in 0..len {
                    let item = self.read_payload(elem_tid)?;
                    list.push(item)?;
                }
                Ok(NbtTag::List(list))
            }
            tag_id::COMPOUND => Ok(NbtTag::Compound(self.read_compound()?)),
            tag_id::INT_ARRAY => {
                let len = self.read_i32()?;
                if len < 0 {
                    return Err(NbtError::NegativeLength(len));
                }
                let count = len as usize;
                let mut out = Vec::with_capacity(count);
                for _ in 0..count {
                    out.push(self.read_i32()?);
                }
                Ok(NbtTag::IntArray(out))
            }
            tag_id::LONG_ARRAY => {
                let len = self.read_i32()?;
                if len < 0 {
                    return Err(NbtError::NegativeLength(len));
                }
                let count = len as usize;
                let mut out = Vec::with_capacity(count);
                for _ in 0..count {
                    out.push(self.read_i64()?);
                }
                Ok(NbtTag::LongArray(out))
            }
            other => Err(NbtError::InvalidTagId(other)),
        }
    }

    fn read_compound(&mut self) -> Result<NbtCompound> {
        let mut compound = NbtCompound::new();
        loop {
            let tid = self.read_u8()?;
            if tid == tag_id::END {
                break;
            }
            let name = self.read_string()?;
            let val = self.read_payload(tid)?;
            compound.insert(name, val);
        }
        Ok(compound)
    }

    fn skip_bytes(&mut self, len: usize) -> Result<()> {
        if self.pos + len > self.data.len() {
            return Err(NbtError::UnexpectedEof);
        }
        self.pos += len;
        Ok(())
    }

    fn skip_string(&mut self) -> Result<()> {
        let len = self.read_u16()? as usize;
        self.skip_bytes(len)
    }

    fn skip_payload(&mut self, tid: u8) -> Result<()> {
        match tid {
            tag_id::BYTE => self.skip_bytes(1),
            tag_id::SHORT => self.skip_bytes(2),
            tag_id::INT | tag_id::FLOAT => self.skip_bytes(4),
            tag_id::LONG | tag_id::DOUBLE => self.skip_bytes(8),
            tag_id::BYTE_ARRAY => {
                let len = self.read_i32()?;
                if len < 0 {
                    return Err(NbtError::NegativeLength(len));
                }
                self.skip_bytes(len as usize)
            }
            tag_id::STRING => self.skip_string(),
            tag_id::LIST => {
                let elem_tid = self.read_u8()?;
                let len = self.read_i32()?;
                if len < 0 {
                    return Err(NbtError::NegativeLength(len));
                }
                let count = len as usize;
                match elem_tid {
                    tag_id::END => Ok(()),
                    tag_id::BYTE => self.skip_bytes(count),
                    tag_id::SHORT => self.skip_bytes(count.saturating_mul(2)),
                    tag_id::INT | tag_id::FLOAT => self.skip_bytes(count.saturating_mul(4)),
                    tag_id::LONG | tag_id::DOUBLE => self.skip_bytes(count.saturating_mul(8)),
                    _ => {
                        for _ in 0..count {
                            self.skip_payload(elem_tid)?;
                        }
                        Ok(())
                    }
                }
            }
            tag_id::COMPOUND => {
                loop {
                    let sub_tid = self.read_u8()?;
                    if sub_tid == tag_id::END {
                        break;
                    }
                    self.skip_string()?;
                    self.skip_payload(sub_tid)?;
                }
                Ok(())
            }
            tag_id::INT_ARRAY => {
                let len = self.read_i32()?;
                if len < 0 {
                    return Err(NbtError::NegativeLength(len));
                }
                self.skip_bytes((len as usize).saturating_mul(4))
            }
            tag_id::LONG_ARRAY => {
                let len = self.read_i32()?;
                if len < 0 {
                    return Err(NbtError::NegativeLength(len));
                }
                self.skip_bytes((len as usize).saturating_mul(8))
            }
            other => Err(NbtError::InvalidTagId(other)),
        }
    }

    fn read_compound_filtered<F>(&mut self, mut keep_key: F) -> Result<NbtCompound>
    where
        F: FnMut(&str) -> bool,
    {
        let mut compound = NbtCompound::new();
        loop {
            let tid = self.read_u8()?;
            if tid == tag_id::END {
                break;
            }
            let name_len = self.read_u16()? as usize;
            let name_bytes = self.read_bytes(name_len)?;
            let keep = match std::str::from_utf8(name_bytes) {
                Ok(s) => keep_key(s),
                Err(_) => {
                    let decoded = decode_mutf8(name_bytes);
                    keep_key(&decoded)
                }
            };
            if keep {
                let name = decode_mutf8(name_bytes);
                let val = self.read_payload(tid)?;
                compound.insert(name, val);
            } else {
                self.skip_payload(tid)?;
            }
        }
        Ok(compound)
    }
}

struct NbtWriter {
    buf: Vec<u8>,
    endian: Endianness,
}

impl NbtWriter {
    fn new(endian: Endianness) -> Self {
        Self {
            buf: Vec::with_capacity(256),
            endian,
        }
    }

    fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    fn write_i8(&mut self, v: i8) {
        self.buf.push(v as u8);
    }

    fn write_u16(&mut self, v: u16) {
        match self.endian {
            Endianness::Big => self.buf.extend_from_slice(&v.to_be_bytes()),
            Endianness::Little => self.buf.extend_from_slice(&v.to_le_bytes()),
        }
    }

    fn write_i16(&mut self, v: i16) {
        self.write_u16(v as u16);
    }

    fn write_i32(&mut self, v: i32) {
        match self.endian {
            Endianness::Big => self.buf.extend_from_slice(&v.to_be_bytes()),
            Endianness::Little => self.buf.extend_from_slice(&v.to_le_bytes()),
        }
    }

    fn write_i64(&mut self, v: i64) {
        match self.endian {
            Endianness::Big => self.buf.extend_from_slice(&v.to_be_bytes()),
            Endianness::Little => self.buf.extend_from_slice(&v.to_le_bytes()),
        }
    }

    fn write_f32(&mut self, v: f32) {
        match self.endian {
            Endianness::Big => self.buf.extend_from_slice(&v.to_be_bytes()),
            Endianness::Little => self.buf.extend_from_slice(&v.to_le_bytes()),
        }
    }

    fn write_f64(&mut self, v: f64) {
        match self.endian {
            Endianness::Big => self.buf.extend_from_slice(&v.to_be_bytes()),
            Endianness::Little => self.buf.extend_from_slice(&v.to_le_bytes()),
        }
    }

    fn write_string(&mut self, s: &str) {
        let bytes = s.as_bytes();
        let len = bytes.len().min(u16::MAX as usize);
        self.write_u16(len as u16);
        self.buf.extend_from_slice(&bytes[..len]);
    }

    fn write_payload(&mut self, tag: &NbtTag) {
        match tag {
            NbtTag::Byte(v) => self.write_i8(*v),
            NbtTag::Short(v) => self.write_i16(*v),
            NbtTag::Int(v) => self.write_i32(*v),
            NbtTag::Long(v) => self.write_i64(*v),
            NbtTag::Float(v) => self.write_f32(*v),
            NbtTag::Double(v) => self.write_f64(*v),
            NbtTag::ByteArray(arr) => {
                self.write_i32(arr.len() as i32);
                for &b in arr {
                    self.buf.push(b as u8);
                }
            }
            NbtTag::String(s) => self.write_string(s),
            NbtTag::List(list) => {
                let elem_tid = if list.is_empty() {
                    tag_id::END
                } else {
                    list.element_tag_id()
                };
                self.write_u8(elem_tid);
                self.write_i32(list.len() as i32);
                for item in list.items() {
                    self.write_payload(item);
                }
            }
            NbtTag::Compound(c) => self.write_compound(c),
            NbtTag::IntArray(arr) => {
                self.write_i32(arr.len() as i32);
                for &v in arr {
                    self.write_i32(v);
                }
            }
            NbtTag::LongArray(arr) => {
                self.write_i32(arr.len() as i32);
                for &v in arr {
                    self.write_i64(v);
                }
            }
        }
    }

    fn write_compound(&mut self, compound: &NbtCompound) {
        for (name, val) in compound.iter() {
            self.write_u8(val.tag_id());
            self.write_string(name);
            self.write_payload(val);
        }
        self.write_u8(tag_id::END);
    }
}

fn decode_mutf8(bytes: &[u8]) -> String {
    // Fast path for valid standard UTF-8 without 0xC0 0x80
    if !bytes.windows(2).any(|w| w == [0xC0, 0x80])
        && let Ok(s) = std::str::from_utf8(bytes)
    {
        return s.to_owned();
    }
    // Fallback handling Java Modified UTF-8 null bytes & lossy sequences
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == 0xC0 && bytes[i + 1] == 0x80 {
            normalized.push(0);
            i += 2;
        } else {
            normalized.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&normalized).into_owned()
}

fn decompress_auto_cow(bytes: &[u8]) -> Result<Cow<'_, [u8]>> {
    if bytes.len() >= 2 && bytes[0] == 0x1F && bytes[1] == 0x8B {
        let mut decoder = GzDecoder::new(Cursor::new(bytes));
        let mut out = Vec::new();
        decoder.read_to_end(&mut out)?;
        return Ok(Cow::Owned(out));
    }
    if bytes.len() >= 2
        && bytes[0] == 0x78
        && ((bytes[0] as u16) * 256 + bytes[1] as u16).is_multiple_of(31)
    {
        let mut decoder = ZlibDecoder::new(Cursor::new(bytes));
        let mut out = Vec::new();
        if decoder.read_to_end(&mut out).is_ok() {
            return Ok(Cow::Owned(out));
        }
    }
    Ok(Cow::Borrowed(bytes))
}

pub fn decompress_auto(bytes: &[u8]) -> Result<Vec<u8>> {
    Ok(decompress_auto_cow(bytes)?.into_owned())
}

pub fn read_nbt(bytes: &[u8], endian: Endianness) -> Result<(String, NbtCompound)> {
    let raw = decompress_auto_cow(bytes)?;
    let mut reader = NbtReader::new(&raw, endian);
    let root_tid = reader.read_u8()?;
    if root_tid != tag_id::COMPOUND {
        return Err(NbtError::InvalidRootTag(root_tid));
    }
    let root_name = reader.read_string()?;
    let compound = reader.read_compound()?;
    Ok((root_name, compound))
}

/// Like `read_nbt`, but skips top-level compound entries where `keep_key(name)` returns `false`
/// without allocating any `String`, `Vec`, or `NbtCompound` for skipped subtrees.
pub fn read_nbt_filtered<F>(
    bytes: &[u8],
    endian: Endianness,
    keep_key: F,
) -> Result<(String, NbtCompound)>
where
    F: FnMut(&str) -> bool,
{
    let raw = decompress_auto_cow(bytes)?;
    let mut reader = NbtReader::new(&raw, endian);
    let root_tid = reader.read_u8()?;
    if root_tid != tag_id::COMPOUND {
        return Err(NbtError::InvalidRootTag(root_tid));
    }
    let root_name = reader.read_string()?;
    let compound = reader.read_compound_filtered(keep_key)?;
    Ok((root_name, compound))
}

pub fn write_nbt(
    root_name: &str,
    compound: &NbtCompound,
    endian: Endianness,
    compression: Compression,
) -> Result<Vec<u8>> {
    let mut writer = NbtWriter::new(endian);
    writer.write_u8(tag_id::COMPOUND);
    writer.write_string(root_name);
    writer.write_compound(compound);

    match compression {
        Compression::None => Ok(writer.buf),
        Compression::Gzip => {
            let mut enc = GzEncoder::new(Vec::new(), FlateCompression::default());
            enc.write_all(&writer.buf)?;
            Ok(enc.finish()?)
        }
        Compression::Zlib => {
            let mut enc = ZlibEncoder::new(Vec::new(), FlateCompression::fast());
            enc.write_all(&writer.buf)?;
            Ok(enc.finish()?)
        }
    }
}

pub fn read_nbt_file(path: impl AsRef<Path>, endian: Endianness) -> Result<(String, NbtCompound)> {
    let bytes = fs::read(path)?;
    read_nbt(&bytes, endian)
}

pub fn write_nbt_file(
    path: impl AsRef<Path>,
    root_name: &str,
    compound: &NbtCompound,
    endian: Endianness,
    compression: Compression,
) -> Result<()> {
    let encoded = write_nbt(root_name, compound, endian, compression)?;
    fs::write(path, encoded)?;
    Ok(())
}
