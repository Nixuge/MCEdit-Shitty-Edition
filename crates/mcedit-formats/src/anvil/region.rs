use flate2::Compression;
use flate2::read::{GzDecoder, ZlibDecoder};
use flate2::write::ZlibEncoder;
use mcedit_core::{FormatError, FormatResult, RepairReport};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SECTOR_BYTES: usize = 4096;
const HEADER_SECTORS: usize = 2;
const CHUNKS_PER_REGION: usize = 1024;
const COMPRESSION_GZIP: u8 = 1;
const COMPRESSION_ZLIB: u8 = 2;

#[derive(Debug)]
pub struct RegionFile {
    path: PathBuf,
    read_file: File,
    file_len: usize,
    offsets: [u32; CHUNKS_PER_REGION],
    timestamps: [u32; CHUNKS_PER_REGION],
    free_sectors: Vec<bool>,
}

impl RegionFile {
    pub fn open_or_create(path: impl AsRef<Path>) -> FormatResult<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.exists() {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = File::create(&path)?;
            file.write_all(&[0u8; SECTOR_BYTES * HEADER_SECTORS])?;
            drop(file);
            let read_file = File::open(&path)?;
            return Ok(Self {
                path,
                read_file,
                file_len: SECTOR_BYTES * HEADER_SECTORS,
                offsets: [0; CHUNKS_PER_REGION],
                timestamps: [0; CHUNKS_PER_REGION],
                free_sectors: vec![false, false],
            });
        }

        let mut file = File::open(&path)?;
        let file_len = file.metadata()?.len() as usize;
        if file_len < SECTOR_BYTES * HEADER_SECTORS {
            // 0-byte or truncated header file (common for empty 1.17+ entities/*.mca files):
            // treat as empty in memory without mutating the file on disk during read-only opens.
            return Ok(Self {
                path,
                read_file: file,
                file_len,
                offsets: [0; CHUNKS_PER_REGION],
                timestamps: [0; CHUNKS_PER_REGION],
                free_sectors: vec![false, false],
            });
        }

        let mut header = [0u8; SECTOR_BYTES * HEADER_SECTORS];
        file.read_exact(&mut header)?;

        let mut offsets = [0u32; CHUNKS_PER_REGION];
        let mut timestamps = [0u32; CHUNKS_PER_REGION];
        for i in 0..CHUNKS_PER_REGION {
            let o = i * 4;
            offsets[i] = u32::from_be_bytes([header[o], header[o + 1], header[o + 2], header[o + 3]]);
            let t = SECTOR_BYTES + i * 4;
            timestamps[i] =
                u32::from_be_bytes([header[t], header[t + 1], header[t + 2], header[t + 3]]);
        }

        let total_sectors = file_len.div_ceil(SECTOR_BYTES).max(HEADER_SECTORS);
        let mut free_sectors = vec![true; total_sectors];
        free_sectors[0] = false;
        free_sectors[1] = false;

        for &off in &offsets {
            if off != 0 {
                let sector = (off >> 8) as usize;
                let count = (off & 0xFF) as usize;
                if sector >= HEADER_SECTORS && sector + count <= total_sectors {
                    free_sectors[sector..sector + count].fill(false);
                }
            }
        }

        Ok(Self {
            path,
            read_file: file,
            file_len,
            offsets,
            timestamps,
            free_sectors,
        })
    }

    #[inline]
    fn index(lx: usize, lz: usize) -> usize {
        (lz & 31) * 32 + (lx & 31)
    }

    pub fn list_present(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for lz in 0..32 {
            for lx in 0..32 {
                if self.offsets[Self::index(lx, lz)] != 0 {
                    out.push((lx, lz));
                }
            }
        }
        out
    }

    #[cfg(unix)]
    fn read_exact_at_offset(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        use std::os::unix::fs::FileExt;
        self.read_file.read_exact_at(buf, offset)
    }

    #[cfg(not(unix))]
    fn read_exact_at_offset(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<()> {
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(buf)
    }

    pub fn read_chunk_nbt(&self, lx: usize, lz: usize) -> FormatResult<Option<Vec<u8>>> {
        let _prof = mcedit_core::profiler::scope(mcedit_core::profiler::Stage::ChunkRead);
        let off = self.offsets[Self::index(lx, lz)];
        if off == 0 {
            return Ok(None);
        }
        let sector = (off >> 8) as usize;
        let count = (off & 0xFF) as usize;
        if sector < HEADER_SECTORS || count == 0 {
            return Err(FormatError::Corrupt(format!(
                "invalid chunk sector offset {sector}:{count} in {}",
                self.path.display()
            )));
        }

        let byte_offset = sector * SECTOR_BYTES;
        if byte_offset + 5 > self.file_len {
            return Err(FormatError::Corrupt(format!(
                "chunk sector {sector} past EOF in {}",
                self.path.display()
            )));
        }

        let mut hdr = [0u8; 5];
        self.read_exact_at_offset(byte_offset as u64, &mut hdr)?;
        let payload_len = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
        if payload_len == 0 || payload_len > count * SECTOR_BYTES {
            return Err(FormatError::Corrupt(format!(
                "invalid chunk payload length {payload_len} (sectors={count}) in {}",
                self.path.display()
            )));
        }

        let comp_type = hdr[4];
        let mut compressed = vec![0u8; payload_len - 1];
        self.read_exact_at_offset((byte_offset + 5) as u64, &mut compressed)?;

        let mut raw = Vec::new();
        match comp_type {
            COMPRESSION_GZIP => {
                let mut dec = GzDecoder::new(&compressed[..]);
                dec.read_to_end(&mut raw)?;
            }
            COMPRESSION_ZLIB => {
                let mut dec = ZlibDecoder::new(&compressed[..]);
                dec.read_to_end(&mut raw)?;
            }
            other => {
                return Err(FormatError::Corrupt(format!(
                    "unknown chunk compression type {other}"
                )));
            }
        }
        Ok(Some(raw))
    }

    pub fn write_chunk_nbt(&mut self, lx: usize, lz: usize, raw_nbt: &[u8]) -> FormatResult<()> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(raw_nbt)?;
        let compressed = enc.finish()?;

        let total_payload = 5 + compressed.len();
        let sectors_needed = total_payload.div_ceil(SECTOR_BYTES).max(1);
        if sectors_needed > 255 {
            return Err(FormatError::Corrupt(
                "chunk exceeds 1MB region sector limit".to_string(),
            ));
        }

        let idx = Self::index(lx, lz);
        let old_off = self.offsets[idx];
        let old_sector = (old_off >> 8) as usize;
        let old_count = (old_off & 0xFF) as usize;

        if old_sector >= HEADER_SECTORS && old_sector + old_count <= self.free_sectors.len() {
            self.free_sectors[old_sector..old_sector + old_count].fill(true);
        }

        // Find contiguous run of `sectors_needed`
        let new_sector = if let Some(s) = self.find_free_run(sectors_needed) {
            s
        } else {
            let start = self.free_sectors.len();
            self.free_sectors.resize(start + sectors_needed, false);
            start
        };
        self.free_sectors[new_sector..new_sector + sectors_needed].fill(false);

        let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        file.seek(SeekFrom::Start((new_sector * SECTOR_BYTES) as u64))?;
        let chunk_len_field = (compressed.len() + 1) as u32;
        file.write_all(&chunk_len_field.to_be_bytes())?;
        file.write_all(&[COMPRESSION_ZLIB])?;
        file.write_all(&compressed)?;
        let pad = sectors_needed * SECTOR_BYTES - total_payload;
        if pad > 0 {
            let zeros = vec![0u8; pad];
            file.write_all(&zeros)?;
        }

        let new_off = ((new_sector as u32) << 8) | (sectors_needed as u32);
        self.offsets[idx] = new_off;
        let ts = now_unix_secs();
        self.timestamps[idx] = ts;

        file.seek(SeekFrom::Start((idx * 4) as u64))?;
        file.write_all(&new_off.to_be_bytes())?;
        file.seek(SeekFrom::Start((SECTOR_BYTES + idx * 4) as u64))?;
        file.write_all(&ts.to_be_bytes())?;
        self.file_len = self
            .file_len
            .max((new_sector + sectors_needed) * SECTOR_BYTES);
        Ok(())
    }

    pub fn delete_chunk(&mut self, lx: usize, lz: usize) -> FormatResult<()> {
        let idx = Self::index(lx, lz);
        let old_off = self.offsets[idx];
        if old_off == 0 {
            return Ok(());
        }
        let old_sector = (old_off >> 8) as usize;
        let old_count = (old_off & 0xFF) as usize;
        if old_sector >= HEADER_SECTORS && old_sector + old_count <= self.free_sectors.len() {
            self.free_sectors[old_sector..old_sector + old_count].fill(true);
        }
        self.offsets[idx] = 0;
        self.timestamps[idx] = 0;

        let mut file = OpenOptions::new().read(true).write(true).open(&self.path)?;
        file.seek(SeekFrom::Start((idx * 4) as u64))?;
        file.write_all(&0u32.to_be_bytes())?;
        file.seek(SeekFrom::Start((SECTOR_BYTES + idx * 4) as u64))?;
        file.write_all(&0u32.to_be_bytes())?;
        Ok(())
    }

    /// Scans for overlapping/OOB sectors or unreadable chunks, then rewrites a clean compacted `.mca`.
    pub fn repair(path: impl AsRef<Path>) -> FormatResult<RepairReport> {
        let path = path.as_ref();
        let mut report = RepairReport {
            files_scanned: 1,
            ..Default::default()
        };
        let rf = Self::open_or_create(path)?;
        let file_bytes = fs::read(path)?;
        let total_sectors = file_bytes.len() / SECTOR_BYTES;

        // Check for overlaps or OOB
        let mut owner: Vec<Option<usize>> = vec![None; total_sectors.max(HEADER_SECTORS)];
        let mut needs_repair = false;
        for (idx, &off) in rf.offsets.iter().enumerate() {
            if off == 0 {
                continue;
            }
            let sector = (off >> 8) as usize;
            let count = (off & 0xFF) as usize;
            if sector < HEADER_SECTORS || count == 0 || sector + count > total_sectors {
                needs_repair = true;
                break;
            }
            for cell in &mut owner[sector..sector + count] {
                if cell.is_some() {
                    needs_repair = true;
                    break;
                }
                *cell = Some(idx);
            }
            if needs_repair {
                break;
            }
        }

        if !needs_repair {
            // Also verify every listed chunk decompresses cleanly
            for (lx, lz) in rf.list_present() {
                if rf.read_chunk_nbt(lx, lz).is_err() {
                    needs_repair = true;
                    break;
                }
            }
        }

        if !needs_repair {
            return Ok(report);
        }

        let tmp_path = path.with_extension("mca.repair_tmp");
        if tmp_path.exists() {
            fs::remove_file(&tmp_path)?;
        }
        let mut clean = Self::open_or_create(&tmp_path)?;
        for (lx, lz) in rf.list_present() {
            match rf.read_chunk_nbt(lx, lz) {
                Ok(Some(raw)) => {
                    clean.write_chunk_nbt(lx, lz, &raw)?;
                    report.chunks_recovered += 1;
                }
                _ => {
                    report.chunks_lost += 1;
                }
            }
        }
        fs::rename(&tmp_path, path)?;
        report.files_repaired = 1;
        Ok(report)
    }

    fn find_free_run(&self, needed: usize) -> Option<usize> {
        let mut run_start = 0usize;
        let mut run_len = 0usize;
        for (i, &is_free) in self.free_sectors.iter().enumerate() {
            if is_free {
                if run_len == 0 {
                    run_start = i;
                }
                run_len += 1;
                if run_len == needed {
                    return Some(run_start);
                }
            } else {
                run_len = 0;
            }
        }
        None
    }
}

fn now_unix_secs() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}
