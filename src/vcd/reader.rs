//! VCD file reader with compression detection
//!
//! Automatically detects and handles compressed VCD files (.gz, .bz2)

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek};
use std::path::Path;

/// Compression format detection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Compression {
    None,
    Gzip,
    Bzip2,
}

impl Compression {
    /// Detect compression from file extension
    #[allow(dead_code)]
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "gz" => Some(Compression::Gzip),
            "bz2" => Some(Compression::Bzip2),
            _ => None,
        }
    }
}

/// Detect compression from file path
#[allow(dead_code)]
pub fn detect_compression(path: &Path) -> Compression {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    if ext.is_empty() {
        return Compression::None;
    }

    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

    if stem.ends_with(".vcd") || stem.ends_with(".vcdump") {
        return Compression::from_extension(ext).unwrap_or(Compression::None);
    }

    Compression::from_extension(ext).unwrap_or(Compression::None)
}

/// Open a file with automatic decompression
#[allow(dead_code)]
pub fn open(path: &Path) -> io::Result<Box<dyn Read>> {
    let compression = detect_compression(path);

    let file = File::open(path)?;
    let reader: Box<dyn Read> = match compression {
        Compression::None => Box::new(file),
        Compression::Gzip => {
            let decoder = flate2::read::GzDecoder::new(file);
            Box::new(decoder)
        }
        Compression::Bzip2 => {
            let decoder = bzip2::read::BzDecoder::new(file);
            Box::new(decoder)
        }
    };

    Ok(reader)
}

/// Create a buffered line reader from a file path
#[allow(dead_code)]
pub fn open_buffered(path: &Path) -> io::Result<BufReader<Box<dyn Read>>> {
    let reader = open(path)?;
    Ok(BufReader::with_capacity(8 * 1024, reader))
}

/// Line reader for VCD files
#[allow(dead_code)]
pub struct LineReader<R: Read> {
    inner: BufReader<R>,
    line: usize,
    buf: String,
}

#[allow(dead_code)]
impl<R: Read> LineReader<R> {
    /// Create a new line reader
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::with_capacity(1024 * 1024, reader), // 1MB buffer
            line: 0,
            buf: String::with_capacity(1024),
        }
    }

    /// Read the next line, returning None on EOF
    pub fn read_line(&mut self) -> io::Result<Option<String>> {
        self.line += 1;
        self.buf.clear();
        match self.inner.read_line(&mut self.buf) {
            Ok(0) => Ok(None),
            Ok(_) => {
                if self.buf.ends_with('\n') {
                    self.buf.pop();
                }
                if self.buf.ends_with('\r') {
                    self.buf.pop();
                }
                Ok(Some(self.buf.clone()))
            }
            Err(e) => Err(e),
        }
    }

    /// Get the current line number
    pub fn line_number(&self) -> usize {
        self.line
    }

    /// Get remaining lines as an iterator
    pub fn lines(&mut self) -> Lines<'_, R> {
        Lines { reader: self }
    }
}

/// Iterator over lines
#[allow(dead_code)]
pub struct Lines<'a, R: Read> {
    reader: &'a mut LineReader<R>,
}

impl<R: Read + Seek> Iterator for Lines<'_, R> {
    type Item = io::Result<(usize, String)>;

    fn next(&mut self) -> Option<Self::Item> {
        let line_num = self.reader.line_number();
        match self.reader.read_line() {
            Ok(Some(line)) => Some(Ok((line_num, line))),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

/// Memory-mapped file reader for VCD files (uncompressed only)
pub struct MmapReader {
    data: memmap2::Mmap,
    pos: usize,
    line: usize,
}

impl MmapReader {
    /// Create a new memory-mapped reader, with kernel hints for sequential access.
    pub fn new(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mmap = unsafe { memmap2::Mmap::map(&file)? };

        // L1: Tell kernel we're reading sequentially → enables 2MB readahead
        #[cfg(target_os = "linux")]
        unsafe {
            let ptr = mmap.as_ptr() as *mut libc::c_void;
            let len = mmap.len();
            // MADV_SEQUENTIAL (2): expect sequential access → aggressive readahead
            // MADV_HUGEPAGE  (14): try to use transparent hugepages
            libc::madvise(ptr, len, libc::MADV_SEQUENTIAL);
            // Ignore errors — best-effort hint
        }

        Ok(Self {
            data: mmap,
            pos: 0,
            line: 0,
        })
    }

    /// Find the next line starting at current position
    /// Returns the line content as a string (without newline)
    #[inline]
    pub fn read_line(&mut self) -> Option<String> {
        if self.pos >= self.data.len() {
            return None;
        }

        self.line += 1;
        let start = self.pos;

        // Find newline (lines are short, simple loop is fast)
        while self.pos < self.data.len() && self.data[self.pos] != b'\n' {
            self.pos += 1;
        }

        // Determine line end (exclusive)
        let end = self.pos;
        let has_crlf = end > start && self.data[end - 1] == b'\r';

        // Skip newline
        if self.pos < self.data.len() {
            self.pos += 1;
        }

        // Extract line content
        let line_end = if has_crlf { end - 1 } else { end };
        let line = &self.data[start..line_end];

        Some(String::from_utf8_lossy(line).to_string())
    }

    /// Read the next line as raw bytes (zero-copy)
    /// L2: memchr SIMD scan on 16KB window — avoids eager page faults
    #[inline]
    pub fn read_line_bytes(&mut self) -> Option<&[u8]> {
        if self.pos >= self.data.len() {
            return None;
        }

        self.line += 1;
        let start = self.pos;
        let window_end = std::cmp::min(self.pos + 16384, self.data.len());
        let window = &self.data[self.pos..window_end];

        // memchr SIMD scans 16 bytes/cycle — fast within page-faulted range
        match memchr::memchr(b'\n', window) {
            Some(nl) => {
                self.pos += nl + 1; // skip newline
                let end = start + nl;
                let has_crlf = end > start && self.data[end - 1] == b'\r';
                let line_end = if has_crlf { end - 1 } else { end };
                Some(&self.data[start..line_end])
            }
            None => {
                // Line longer than 16KB (unusual in VCD) — fall back to byte scan
                self.pos = window_end;
                while self.pos < self.data.len() && self.data[self.pos] != b'\n' {
                    self.pos += 1;
                }
                if self.pos >= self.data.len() {
                    return Some(&self.data[start..]);
                }
                self.pos += 1;
                let end = self.pos - 1;
                let has_crlf = end > start && self.data[end - 1] == b'\r';
                let line_end = if has_crlf { end - 1 } else { end };
                Some(&self.data[start..line_end])
            }
        }
    }

    /// Seek to an absolute byte offset in the file
    #[inline]
    pub fn seek_to(&mut self, offset: u64) -> io::Result<()> {
        if offset as usize > self.data.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "offset beyond file end"));
        }
        self.pos = offset as usize;
        Ok(())
    }

    /// Get the current byte offset
    #[inline]
    pub fn current_offset(&self) -> u64 {
        self.pos as u64
    }

    /// Get a reference to the mmap'd data
    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Get the total length of the mmap'd data
    #[inline]
    pub fn data_len(&self) -> usize {
        self.data.len()
    }

    /// Get the current line number
    pub fn line_number(&self) -> usize {
        self.line
    }
}

impl Read for MmapReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.data.len() {
            return Ok(0);
        }
        let n = std::cmp::min(buf.len(), self.data.len() - self.pos);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Seek for MmapReader {
    fn seek(&mut self, pos: io::SeekFrom) -> io::Result<u64> {
        let new_pos = match pos {
            io::SeekFrom::Start(p) => p as i64,
            io::SeekFrom::End(p) => self.data.len() as i64 + p,
            io::SeekFrom::Current(p) => self.pos as i64 + p,
        };
        if new_pos < 0 || new_pos as usize > self.data.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid seek position"));
        }
        self.pos = new_pos as usize;
        Ok(self.pos as u64)
    }
}

/// Open a file with automatic decompression, or memory-map if uncompressed
#[allow(dead_code)]
pub fn open_mmap(path: &Path) -> io::Result<Box<dyn Read>> {
    let compression = detect_compression(path);

    match compression {
        Compression::None => {
            let mmap_reader = MmapReader::new(path)?;
            Ok(Box::new(mmap_reader))
        }
        Compression::Gzip | Compression::Bzip2 => {
            let reader = open(path)?;
            Ok(reader)
        }
    }
}

/// File metadata
#[allow(dead_code)]
pub struct FileInfo {
    pub path: String,
    pub size: u64,
    pub compression: Compression,
}

#[allow(dead_code)]
impl FileInfo {
    pub fn from_path(path: &Path) -> io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            path: path.to_string_lossy().to_string(),
            size: metadata.len(),
            compression: detect_compression(path),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_compression() {
        assert_eq!(detect_compression(Path::new("test.vcd")), Compression::None);
        assert_eq!(detect_compression(Path::new("test.vcd.gz")), Compression::Gzip);
        assert_eq!(detect_compression(Path::new("test.vcd.bz2")), Compression::Bzip2);
        assert_eq!(detect_compression(Path::new("test.gz")), Compression::Gzip);
    }
}
