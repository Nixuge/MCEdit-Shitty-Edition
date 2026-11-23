use thiserror::Error;

#[derive(Debug, Error)]
pub enum NbtError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected EOF while reading NBT")]
    UnexpectedEof,
    #[error("invalid NBT tag id: {0}")]
    InvalidTagId(u8),
    #[error("expected root TAG_Compound (10), found tag id {0}")]
    InvalidRootTag(u8),
    #[error("negative NBT length: {0}")]
    NegativeLength(i32),
    #[error("list element type mismatch: expected tag {expected}, found {found}")]
    ListTypeMismatch { expected: u8, found: u8 },
}

pub type Result<T> = std::result::Result<T, NbtError>;
