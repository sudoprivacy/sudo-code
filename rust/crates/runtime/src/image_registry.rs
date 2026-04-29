use std::fs;
use std::io::{self, Cursor};
use std::path::PathBuf;

use base64::Engine;
use sha2::{Digest, Sha256};

/// Maximum raw image size before downsampling (5 MB).
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Maximum image dimension (width or height) before downsampling.
const MAX_IMAGE_DIMENSION: u32 = 8000;

/// On-disk cache for pasted images, keyed by their SHA-256 content hash.
///
/// Images are stored under `~/.config/scode/image_cache/<hash>.<ext>` so that
/// identical pastes are deduplicated and can be referenced by a short
/// `<image:HASH>` tag inside the prompt text.
pub struct ImageRegistry {
    cache_dir: PathBuf,
}

/// Information about a successfully registered image.
#[derive(Debug, Clone)]
pub struct RegisteredImage {
    /// Hex-encoded SHA-256 of the (possibly down-sampled) image bytes.
    pub hash: String,
    /// MIME type, e.g. `image/png`.
    pub mime_type: String,
    /// On-disk path inside the cache directory.
    pub path: PathBuf,
}

impl ImageRegistry {
    /// Create a new registry backed by the default cache directory
    /// (`~/.config/scode/image_cache`).
    pub fn default_cache() -> io::Result<Self> {
        let base = dirs_next().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "cannot determine home directory")
        })?;
        let cache_dir = base.join(".config").join("scode").join("image_cache");
        fs::create_dir_all(&cache_dir)?;
        Ok(Self { cache_dir })
    }

    /// Register raw RGBA image data (as provided by `arboard`).
    ///
    /// The image is encoded to PNG, optionally downsampled if it exceeds size
    /// or dimension limits, and stored on disk.  Returns metadata including the
    /// content hash that can be embedded as `<image:HASH>` in the prompt.
    pub fn register_rgba(
        &self,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> io::Result<RegisteredImage> {
        let png_bytes = encode_rgba_to_png(width, height, rgba)?;
        let (final_bytes, mime_type) = maybe_downsample(&png_bytes)?;
        self.store(&final_bytes, &mime_type)
    }

    /// Register already-encoded image bytes (PNG, JPEG, etc.).
    pub fn register_bytes(&self, bytes: &[u8], mime_type: &str) -> io::Result<RegisteredImage> {
        let (final_bytes, final_mime) = maybe_downsample_raw(bytes, mime_type)?;
        self.store(&final_bytes, &final_mime)
    }

    /// Load a previously stored image by its hex hash. Returns `(base64_data, mime_type)`.
    pub fn load(&self, hash: &str) -> io::Result<(String, String)> {
        let entry = self.find_by_hash(hash)?;
        let bytes = fs::read(&entry.0)?;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok((b64, entry.1))
    }

    /// Check whether an image with this hash already exists.
    #[must_use]
    pub fn exists(&self, hash: &str) -> bool {
        self.find_by_hash(hash).is_ok()
    }

    // --- internal helpers ---

    fn store(&self, bytes: &[u8], mime_type: &str) -> io::Result<RegisteredImage> {
        let hash = hex_sha256(bytes);
        let ext = mime_to_ext(mime_type);
        let filename = format!("{hash}.{ext}");
        let path = self.cache_dir.join(&filename);

        if !path.exists() {
            fs::write(&path, bytes)?;
        }

        Ok(RegisteredImage {
            hash,
            mime_type: mime_type.to_string(),
            path,
        })
    }

    fn find_by_hash(&self, hash: &str) -> io::Result<(PathBuf, String)> {
        for ext in &["png", "jpg", "jpeg", "gif", "webp"] {
            let path = self.cache_dir.join(format!("{hash}.{ext}"));
            if path.exists() {
                let mime = ext_to_mime(ext);
                return Ok((path, mime.to_string()));
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("image not found for hash {hash}"),
        ))
    }
}

fn hex_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

fn mime_to_ext(mime: &str) -> &str {
    match mime {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    }
}

fn ext_to_mime(ext: &str) -> &str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    }
}

fn encode_rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> io::Result<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "RGBA buffer size does not match dimensions",
        )
    })?;
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png)
        .map_err(io::Error::other)?;
    Ok(buf.into_inner())
}

/// Downsample an already-encoded image if it exceeds size or dimension limits.
fn maybe_downsample_raw(bytes: &[u8], mime_type: &str) -> io::Result<(Vec<u8>, String)> {
    if bytes.len() <= MAX_IMAGE_BYTES {
        let img = image::load_from_memory(bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if img.width() <= MAX_IMAGE_DIMENSION && img.height() <= MAX_IMAGE_DIMENSION {
            return Ok((bytes.to_vec(), mime_type.to_string()));
        }
    }
    // Need to downsample
    let img = image::load_from_memory(bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    downsample_image(img)
}

/// Downsample a PNG-encoded buffer if needed.
fn maybe_downsample(png_bytes: &[u8]) -> io::Result<(Vec<u8>, String)> {
    maybe_downsample_raw(png_bytes, "image/png")
}

/// Resize an image so that neither dimension exceeds `MAX_IMAGE_DIMENSION` and
/// the re-encoded JPEG output fits within `MAX_IMAGE_BYTES`.
fn downsample_image(img: image::DynamicImage) -> io::Result<(Vec<u8>, String)> {
    let mut current = img;

    // First pass: resize if dimensions are too large.
    if current.width() > MAX_IMAGE_DIMENSION || current.height() > MAX_IMAGE_DIMENSION {
        current = current.resize(
            MAX_IMAGE_DIMENSION,
            MAX_IMAGE_DIMENSION,
            image::imageops::FilterType::Lanczos3,
        );
    }

    // Encode as JPEG (much smaller than PNG for photos).
    let mut quality = 85u8;
    loop {
        let mut buf = Cursor::new(Vec::new());
        let jpeg_enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
        current
            .write_with_encoder(jpeg_enc)
            .map_err(io::Error::other)?;
        let encoded = buf.into_inner();

        if encoded.len() <= MAX_IMAGE_BYTES || quality <= 30 {
            return Ok((encoded, "image/jpeg".to_string()));
        }

        // Reduce quality and try again.
        quality = quality.saturating_sub(15);
    }
}

/// Resolve the user's home directory.
fn dirs_next() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn temp_registry() -> ImageRegistry {
        let dir = env::temp_dir().join(format!(
            "scode_image_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        ImageRegistry { cache_dir: dir }
    }

    #[test]
    fn register_and_load_round_trip() {
        let registry = temp_registry();
        // Create a tiny 2x2 RGBA image.
        let rgba = vec![255u8; 2 * 2 * 4];
        let registered = registry.register_rgba(2, 2, &rgba).unwrap();
        assert!(!registered.hash.is_empty());
        assert_eq!(registered.mime_type, "image/png");
        assert!(registered.path.exists());

        let (b64, mime) = registry.load(&registered.hash).unwrap();
        assert_eq!(mime, "image/png");
        assert!(!b64.is_empty());
    }

    #[test]
    fn deduplication_reuses_existing_file() {
        let registry = temp_registry();
        let rgba = vec![128u8; 4 * 4 * 4];
        let first = registry.register_rgba(4, 4, &rgba).unwrap();
        let second = registry.register_rgba(4, 4, &rgba).unwrap();
        assert_eq!(first.hash, second.hash);
        assert_eq!(first.path, second.path);
    }

    #[test]
    fn nonexistent_hash_returns_error() {
        let registry = temp_registry();
        let result = registry.load("deadbeef");
        assert!(result.is_err());
    }
}
