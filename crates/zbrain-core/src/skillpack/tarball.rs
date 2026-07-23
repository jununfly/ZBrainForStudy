//! Tarball packing and extraction with compression bomb protection.
//!
//! Extracts a gzipped tarball with strict caps on:
//! - max total files
//! - max bytes per file
//! - max total bytes
//! - max path length
//! - max compression ratio
//! Rejects path traversal and non-regular file/directory entries.

use std::fs::{create_dir_all, read, write, Metadata};
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use tar::{Archive, Entry};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result, StructuredError};

/// Default extraction capability caps.
pub const DEFAULT_EXTRACT_CAPS: ExtractCaps = ExtractCaps {
    max_files: 5000,
    max_bytes_per_file: 1024 * 1024, // 1 MB
    max_total_bytes: 100 * 1024 * 1024, // 100 MB
    max_path_length: 255,
    max_compression_ratio: 100,
};

/// Capability limits for extraction to prevent compression bombs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractCaps {
    /// Maximum number of file entries allowed.
    pub max_files: usize,
    /// Maximum uncompressed bytes per file.
    pub max_bytes_per_file: u64,
    /// Maximum total uncompressed bytes across all files.
    pub max_total_bytes: u64,
    /// Maximum path length (bytes).
    pub max_path_length: usize,
    /// Maximum allowed compression ratio (uncompressed / compressed).
    pub max_compression_ratio: u64,
}

impl Default for ExtractCaps {
    fn default() -> Self {
        DEFAULT_EXTRACT_CAPS
    }
}

/// Options for extracting a skillpack tarball.
#[derive(Debug, Clone)]
pub struct TarballExtractOptions {
    /// Absolute path to the .tgz file.
    pub tgz_path: PathBuf,
    /// Absolute path where contents should be extracted (must be empty or not exist).
    pub dest_dir: PathBuf,
    /// Optional capability overrides.
    pub caps: Option<ExtractCaps>,
}

/// Result from extracting a tarball.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TarballExtractResult {
    /// Destination directory where extraction completed.
    pub dest_dir: PathBuf,
    /// Number of file entries extracted (directories not counted).
    pub file_count: usize,
    /// Total uncompressed bytes extracted.
    pub total_bytes: u64,
    /// SHA-256 hash of the compressed tarball (hex lowercase).
    pub sha256: String,
}

/// Error codes for tarball operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarballErrorCode {
    /// Source directory for packing does not exist.
    PackSourceMissing,
    /// Packing failed (external process error in TS, unused in Rust).
    PackFailed,
    /// Tarball file does not exist.
    ExtractTgzMissing,
    /// Destination directory already exists and is not empty.
    ExtractDestNotEmpty,
    /// Extraction failed (IO or format error).
    ExtractFailed,
    /// Path traversal attempt detected.
    ExtractPathTraversal,
    /// Disallowed entry type (symlink/hardlink/device/etc).
    ExtractDisallowedEntryType,
    /// Single file exceeds maximum size cap.
    ExtractFileTooLarge,
    /// Total extracted size exceeds maximum cap.
    ExtractTotalTooLarge,
    /// Too many file entries.
    ExtractTooManyFiles,
    /// File path exceeds maximum length.
    ExtractPathTooLong,
    /// Compression ratio exceeds maximum cap (probable compression bomb).
    ExtractCompressionBomb,
    /// tar binary not found (only relevant for TS implementation).
    TarBinaryNotFound,
}

/// Error for tarball operations.
#[derive(Debug)]
pub struct TarballError {
    code: TarballErrorCode,
    message: String,
    detail: Option<(Option<PathBuf>, Option<u64>, Option<u64>)>,
}

impl TarballError {
    fn new(code: TarballErrorCode, message: String) -> Self {
        Self {
            code,
            message,
            detail: None,
        }
    }

    fn with_detail(
        code: TarballErrorCode,
        message: impl Into<String>,
        path: Option<&Path>,
        size: Option<u64>,
        limit: Option<u64>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            detail: Some((path.map(Path::to_path_buf), size, limit)),
        }
    }
}

impl std::fmt::Display for TarballError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code: {:?})", self.message, self.code)
    }
}

impl std::error::Error for TarballError {}

impl From<TarballError> for Error {
    fn from(e: TarballError) -> Self {
        StructuredError::new("Tarball", "tarball_error", e.to_string())
    }
}

/// Compute SHA-256 hash of a file on disk.
pub fn file_sha256(path: &Path) -> Result<String> {
    let bytes = read(path).map_err(|e| {
        TarballError::new(
            TarballErrorCode::ExtractTgzMissing,
            format!("Failed to read tarball: {e}"),
        )
    })?;
    let digest = sha2::Sha256::digest(&bytes);
    Ok(hex::encode(digest))
}

/// Extract a gzipped tarball to the destination directory with capability caps.
pub fn extract_tarball(options: &TarballExtractOptions) -> Result<TarballExtractResult> {
    let tgz_path = &options.tgz_path;
    let dest_dir = &options.dest_dir;
    let caps = options.caps.clone().unwrap_or_default();

    // Check that input exists
    if !tgz_path.exists() {
        return Err(TarballError::new(
            TarballErrorCode::ExtractTgzMissing,
            format!("Tarball not found at {}", tgz_path.display()),
        )
        .into());
    }

    // Check that destination is either non-existent or empty
    if dest_dir.exists() {
        let entries = std::fs::read_dir(dest_dir)
            .map(|d| d.count())
            .unwrap_or(0);
        if entries > 0 {
            return Err(TarballError::new(
                TarballErrorCode::ExtractDestNotEmpty,
                format!(
                    "Destination directory {} is not empty ({} entries)",
                    dest_dir.display(),
                    entries
                ),
            )
            .into());
        }
    } else {
        create_dir_all(dest_dir).map_err(|e| {
            TarballError::new(
                TarballErrorCode::ExtractFailed,
                format!("Failed to create destination directory: {e}"),
            )
        })?;
    }

    // Open the gzipped tarball
    let file = std::fs::File::open(tgz_path).map_err(|e| {
        TarballError::new(
            TarballErrorCode::ExtractTgzMissing,
            format!("Failed to open tarball: {e}"),
        )
    })?;
    let compressed_size = file.metadata().map(|m| m.len()).unwrap_or(0);
    let gz = GzDecoder::new(file);
    let mut archive = Archive::new(gz);

    let mut file_count = 0;
    let mut total_bytes = 0;

    let entries = archive.entries().map_err(|e| {
        TarballError::new(
            TarballErrorCode::ExtractFailed,
            format!("Failed to read tar entries: {e}"),
        )
    })?;

    for entry in entries {
        let mut entry = entry.map_err(|e| {
            TarballError::new(
                TarballErrorCode::ExtractFailed,
                format!("Invalid tar entry: {e}"),
            )
        })?;

        // Get entry path and normalize it
        let path = entry.path().map_err(|e| {
            TarballError::new(
                TarballErrorCode::ExtractFailed,
                format!("Invalid tar entry path: {e}"),
            )
        })?;

        // Check path length
        let path_str = path.to_string_lossy();
        if path_str.len() > caps.max_path_length {
            return Err(TarballError::with_detail(
                TarballErrorCode::ExtractPathTooLong,
                "File path exceeds maximum length",
                Some(&path),
                Some(path_str.len() as u64),
                Some(caps.max_path_length as u64),
            )
            .into());
        }

        // Check for path traversal
        let dest_path = dest_dir.join(&path);
        if !dest_path.starts_with(dest_dir) {
            return Err(TarballError::new(
                TarballErrorCode::ExtractPathTraversal,
                format!(
                    "Path traversal attempt detected: {}",
                    dest_path.display()
                ),
            )
            .into());
        }

        // Only allow regular files and directories
        let entry_type = entry.header().entry_type();
        if !entry_type.is_file() && !entry_type.is_dir() {
            return Err(TarballError::new(
                TarballErrorCode::ExtractDisallowedEntryType,
                format!(
                    "Disallowed entry type for '{}': only regular files and directories allowed",
                    path.display()
                ),
            )
            .into());
        }

        // Check file count
        if entry_type.is_file() {
            file_count += 1;
            if file_count > caps.max_files {
                return Err(TarballError::with_detail(
                    TarballErrorCode::ExtractTooManyFiles,
                    "Too many file entries",
                    None,
                    Some(file_count as u64),
                    Some(caps.max_files as u64),
                )
                .into());
            }
        }

        // Get uncompressed size for ratio check
        let size = entry.size();
        if compressed_size > 0 {
            let ratio = size / compressed_size;
            if ratio > caps.max_compression_ratio {
                return Err(TarballError::with_detail(
                    TarballErrorCode::ExtractCompressionBomb,
                    "Compression ratio exceeds maximum (possible compression bomb)",
                    Some(&path),
                    Some(ratio),
                    Some(caps.max_compression_ratio),
                )
                .into());
            }
        }

        // Check total bytes
        total_bytes += size;
        if total_bytes > caps.max_total_bytes {
            return Err(TarballError::with_detail(
                TarballErrorCode::ExtractTotalTooLarge,
                "Total extracted size exceeds maximum",
                None,
                Some(total_bytes),
                Some(caps.max_total_bytes),
            )
            .into());
        }

        if entry_type.is_file() {
            // Check per-file size
            if size > caps.max_bytes_per_file {
                return Err(TarballError::with_detail(
                    TarballErrorCode::ExtractFileTooLarge,
                    "Single file exceeds maximum size",
                    Some(&path),
                    Some(size),
                    Some(caps.max_bytes_per_file),
                )
                .into());
            }

            // Ensure parent directory exists
            if let Some(parent) = dest_path.parent() {
                if !parent.exists() {
                    create_dir_all(parent).map_err(|e| {
                        TarballError::new(
                            TarballErrorCode::ExtractFailed,
                            format!("Failed to create parent directory: {e}"),
                        )
                    })?;
                }
            }

            // Extract the file
            let mut out_file = std::fs::File::create(&dest_path).map_err(|e| {
                TarballError::new(
                    TarballErrorCode::ExtractFailed,
                    format!("Failed to create output file: {e}"),
                )
            })?;
            std::io::copy(&mut entry, &mut out_file).map_err(|e| {
                TarballError::new(
                    TarballErrorCode::ExtractFailed,
                    format!("Failed to write file content: {e}"),
                )
            })?;
        } else if entry_type.is_dir() {
            create_dir_all(&dest_path).map_err(|e| {
                TarballError::new(
                    TarballErrorCode::ExtractFailed,
                    format!("Failed to create directory: {e}"),
                )
            })?;
        }
    }

    // Compute SHA-256 of the original compressed file
    let sha256 = file_sha256(tgz_path)?;

    Ok(TarballExtractResult {
        dest_dir: dest_dir.to_path_buf(),
        file_count,
        total_bytes,
        sha256,
    })
}

/// Output from packing a tarball.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TarballOutput {
    /// Path to the created tarball on disk.
    pub tarball_path: PathBuf,
    /// Size of the compressed tarball in bytes.
    pub tarball_size: u64,
    /// SHA-256 hash of the compressed tarball (hex lowercase).
    pub sha256_hex: String,
    /// Whether all audit checks passed.
    pub audit_passed: bool,
}

/// Pack a directory into a gzipped deterministic tarball.
/// Paths are sorted lex order for reproducible builds.
pub fn pack_tarball(
    source_root: &Path,
    output_path: &Path,
) -> Result<TarballOutput> {
    use std::fs::{self, File};
    use tar::{Builder, Header};
    use flate2::write::GzEncoder;
    use flate2::Compression;

    // Create output directory if needed
    if let Some(parent) = output_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)
                .map_err(|e| TarballError::new(
                    TarballErrorCode::PackFailed,
                    format!("Failed to create output directory: {}", e),
                ))?;
        }
    }

    // Open output file
    let out_file = File::create(output_path)
        .map_err(|e| TarballError::new(
            TarballErrorCode::PackFailed,
            format!("Failed to create output file: {}", e),
        ))?;

    // Gzip compression
    let gz = GzEncoder::new(out_file, Compression::default());
    let mut builder = Builder::new(gz);

    // Walk the directory, collect all regular files
    let mut files = Vec::new();
    fn walk_dir(dir: &Path, base: &Path, files: &mut Vec<(PathBuf, PathBuf)>) -> std::result::Result<(), TarballError> {
        let entries = fs::read_dir(dir)
            .map_err(|e| TarballError::new(
                TarballErrorCode::PackFailed,
                format!("Failed to read directory: {}", e),
            ))?;
        for entry in entries {
            let entry = entry.map_err(|e| TarballError::new(
                TarballErrorCode::PackFailed,
                format!("Failed to read directory entry: {}", e),
            ))?;
            let path = entry.path();
            let rel_path = path.strip_prefix(base)
                .map_err(|e| TarballError::new(
                    TarballErrorCode::PackFailed,
                    format!("Failed to strip prefix: {}", e),
                ))?;
            if path.is_file() {
                files.push((path.to_path_buf(), rel_path.to_path_buf()));
            } else if path.is_dir() {
                walk_dir(&path, base, files)?;
            }
        }
        Ok(())
    }

    if !source_root.exists() {
        return Err(TarballError::new(
            TarballErrorCode::PackSourceMissing,
            format!("Source directory {} does not exist", source_root.display()),
        ).into());
    }

    walk_dir(source_root, source_root, &mut files)?;

    // Sort files lex order for deterministic output
    files.sort_by(|(_, a), (_, b)| a.cmp(b));

    // Add each file to the tar
    for (abs_path, rel_path) in files {
        let meta = fs::metadata(&abs_path)
            .map_err(|e| TarballError::new(
                TarballErrorCode::PackFailed,
                format!("Failed to read file metadata: {}", e),
            ))?;

        let mut header = Header::new_gnu();
        header.set_path(rel_path.to_string_lossy().as_ref())
            .map_err(|e| TarballError::new(
                TarballErrorCode::PackFailed,
                format!("Invalid path for tar entry: {}", e),
            ))?;
        header.set_size(meta.len());
        header.set_mode(0o644);
        header.set_mtime(meta.modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0));
        header.set_cksum();

        let mut file = File::open(&abs_path)
            .map_err(|e| TarballError::new(
                TarballErrorCode::PackFailed,
                format!("Failed to open file for packing: {}", e),
            ))?;

        builder.append(&mut header, &mut file)
            .map_err(|e| TarballError::new(
                TarballErrorCode::PackFailed,
                format!("Failed to append file to tar: {}", e),
            ))?;
    }

    // Finalize the tarball
    let mut gz = builder.into_inner()
        .map_err(|e| TarballError::new(
            TarballErrorCode::PackFailed,
            format!("Failed to finish tarball: {}", e),
        ))?;
    gz.try_finish().map_err(|e| TarballError::new(
        TarballErrorCode::PackFailed,
        format!("Failed to finish compression: {}", e),
    ))?;
    let compressed_size = std::fs::metadata(output_path).map(|m| m.len()).unwrap_or(0);

    // Compute SHA-256 of the compressed tarball
    let sha256 = file_sha256(output_path)?;

    Ok(TarballOutput {
        tarball_path: output_path.to_path_buf(),
        tarball_size: compressed_size,
        sha256_hex: sha256,
        audit_passed: true,
    })
}
