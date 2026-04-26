//! Compression interface for FST blocks
//!
//! Supports multiple compression algorithms:
//! - LZ4: fastest, best for real-time processing
//! - zlib: balanced compression ratio

use super::types::Compression;
use std::io::{Read, Result};

/// Compression trait - all compressors must implement this
#[allow(dead_code)]
pub trait Compressor: Send + Sync {
    /// Compress input data
    fn compress(&self, input: &[u8]) -> Vec<u8>;

    /// Decompress input data (if supported)
    fn decompress(&self, input: &[u8], output_size: usize) -> Result<Vec<u8>>;

    /// Algorithm name
    fn name(&self) -> &'static str;
}

/// LZ4 compressor - fastest compression
#[allow(dead_code)]
pub struct Lz4Compressor;

impl Lz4Compressor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Lz4Compressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Compressor for Lz4Compressor {
    fn compress(&self, input: &[u8]) -> Vec<u8> {
        lz4_flex::block::compress_prepend_size(input)
    }

    fn decompress(&self, input: &[u8], output_size: usize) -> Result<Vec<u8>> {
        let decompressed = lz4_flex::block::decompress_size_prepended(input)
            .map_err(|_| std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "LZ4 decompression failed",
            ))?;
        if decompressed.len() != output_size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Decompression size mismatch",
            ));
        }
        Ok(decompressed)
    }

    fn name(&self) -> &'static str {
        "LZ4"
    }
}

/// zlib compressor using flate2
#[allow(dead_code)]
pub struct ZlibCompressor {
    level: CompressionLevel,
}

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum CompressionLevel {
    Fast = 1,
    Balanced = 6,
    Best = 9,
}

impl Default for ZlibCompressor {
    fn default() -> Self {
        Self {
            level: CompressionLevel::Balanced,
        }
    }
}

#[allow(dead_code)]
impl ZlibCompressor {
    pub fn new(level: CompressionLevel) -> Self {
        Self { level }
    }
}

impl Compressor for ZlibCompressor {
    fn compress(&self, input: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::ZlibEncoder::new(
            Vec::with_capacity(input.len()),
            match self.level {
                CompressionLevel::Fast => flate2::Compression::fast(),
                CompressionLevel::Balanced => flate2::Compression::default(),
                CompressionLevel::Best => flate2::Compression::best(),
            },
        );
        use std::io::Write;
        encoder.write_all(input).unwrap();
        encoder.finish().unwrap()
    }

    fn decompress(&self, input: &[u8], output_size: usize) -> Result<Vec<u8>> {
        let mut decoder = flate2::read::ZlibDecoder::new(input);
        let mut output = vec![0u8; output_size];
        decoder.read_exact(&mut output)?;
        Ok(output)
    }

    fn name(&self) -> &'static str {
        "zlib"
    }
}

/// Get compressor instance based on algorithm selection
#[allow(dead_code)]
pub fn get_compressor(compression: Compression) -> Box<dyn Compressor> {
    match compression {
        Compression::Lz4 => Box::new(Lz4Compressor::new()),
        Compression::Zlib => Box::new(ZlibCompressor::default()),
        Compression::FastLz => Box::new(Lz4Compressor::new()), // Fallback to LZ4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lz4_roundtrip() {
        let data = b"Hello, this is a test of the LZ4 compression!";
        let compressor = Lz4Compressor::new();
        let compressed = compressor.compress(data);
        let decompressed = compressor.decompress(&compressed, data.len()).unwrap();
        assert_eq!(&decompressed[..], data);
    }

    #[test]
    fn test_zlib_roundtrip() {
        let data = b"Hello, this is a test of the zlib compression!";
        let compressor = ZlibCompressor::default();
        let compressed = compressor.compress(data);
        let decompressed = compressor.decompress(&compressed, data.len()).unwrap();
        assert_eq!(&decompressed[..], data);
    }
}
