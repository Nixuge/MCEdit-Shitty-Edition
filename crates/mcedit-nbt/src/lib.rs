pub mod error;
pub mod io;
pub mod value;

pub use error::{NbtError, Result};
pub use io::{
    decompress_auto, read_nbt, read_nbt_file, read_nbt_filtered, write_nbt, write_nbt_file,
};
pub use value::{Compression, Endianness, NbtCompound, NbtList, NbtTag, tag_id, tag_type_name};

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_compound() -> NbtCompound {
        let mut root = NbtCompound::new();
        root.insert("byte", NbtTag::Byte(-42));
        root.insert("short", NbtTag::Short(-1234));
        root.insert("int", NbtTag::Int(123456789));
        root.insert("long", NbtTag::Long(-9876543210123));
        root.insert("float", NbtTag::Float(3.5));
        root.insert("double", NbtTag::Double(-42.125));
        root.insert("byte_arr", NbtTag::ByteArray(vec![1, -2, 127, -128]));
        root.insert("string", NbtTag::String("Hello 世界!".to_string()));
        root.insert("int_arr", NbtTag::IntArray(vec![0, -1, 42, i32::MAX]));
        root.insert("long_arr", NbtTag::LongArray(vec![i64::MIN, 0, i64::MAX]));

        let list = NbtList::from_items(vec![
            NbtTag::String("a".to_string()),
            NbtTag::String("b".to_string()),
        ])
        .unwrap();
        root.insert("list", list);
        root.insert("empty_list", NbtList::new_empty());

        let mut nested = NbtCompound::new();
        nested.insert("x", 10i32);
        nested.insert("y", 64i32);
        nested.insert("z", -20i32);
        root.insert("nested", nested);
        root
    }

    #[test]
    fn roundtrip_all_tags_endian_and_compression() {
        let root = sample_compound();
        for endian in [Endianness::Big, Endianness::Little] {
            for comp in [Compression::None, Compression::Gzip, Compression::Zlib] {
                let bytes = write_nbt("TestRoot", &root, endian, comp).unwrap();
                let (name, parsed) = read_nbt(&bytes, endian).unwrap();
                assert_eq!(name, "TestRoot");
                assert_eq!(parsed, root);
            }
        }
    }

    #[test]
    fn preserves_key_order_and_rename() {
        let mut c = NbtCompound::new();
        c.insert("first", 1i32);
        c.insert("second", 2i32);
        c.insert("third", 3i32);
        assert!(c.rename_key("second", "middle"));
        let keys: Vec<_> = c.keys().cloned().collect();
        assert_eq!(keys, vec!["first", "middle", "third"]);
        assert_eq!(c.get_int("middle"), Some(2));
    }

    #[test]
    fn rejects_invalid_and_truncated_input() {
        assert!(matches!(
            read_nbt(&[], Endianness::Big),
            Err(NbtError::UnexpectedEof)
        ));
        assert!(matches!(
            read_nbt(&[tag_id::INT, 0, 0, 0, 0, 0, 1], Endianness::Big),
            Err(NbtError::InvalidRootTag(tag_id::INT))
        ));
        // Negative list length
        let bad = [tag_id::COMPOUND, 0, 0, tag_id::LIST, 0, 1, b'a', tag_id::BYTE, 0xff, 0xff, 0xff, 0xff];
        assert!(matches!(
            read_nbt(&bad, Endianness::Big),
            Err(NbtError::NegativeLength(-1))
        ));
    }
}
