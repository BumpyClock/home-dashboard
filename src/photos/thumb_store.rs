//! Grid thumbnails persisted as small JPEGs, so each photo version is decoded at full size once.

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageFormat, ImageReader};
use slint::{Rgb8Pixel, SharedPixelBuffer};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

// Bump when the stored rendition changes (size, crop, encoding) so old files stop matching.
const FORMAT: u64 = 1;
const QUALITY: u8 = 90;
const MAX_STORED_BYTES: u64 = 1024 * 1024;
const NAME_HEX_DIGITS: usize = 16;
const EXTENSION: &str = ".jpg";

pub struct ThumbStore {
    dir: PathBuf,
}

impl ThumbStore {
    /// `FRAME_THUMB_CACHE` selects the directory and an empty value disables the store.
    /// Otherwise the store lives under `$XDG_CACHE_HOME` or `~/.cache`.
    pub fn from_env() -> Option<Self> {
        let dir = match std::env::var_os("FRAME_THUMB_CACHE") {
            Some(value) if value.is_empty() => return None,
            Some(value) => PathBuf::from(value),
            None => std::env::var_os("XDG_CACHE_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME")
                        .filter(|value| !value.is_empty())
                        .map(|home| PathBuf::from(home).join(".cache"))
                })?
                .join("family-frame/thumbs"),
        };
        Some(Self::new(dir))
    }

    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// FNV-1a over the source identity; unlike `DefaultHasher`, stable across Rust releases.
    pub fn file_name(source: &Path, modified: SystemTime, length: u64, side: u32) -> String {
        let nanos = modified
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for bytes in [
            source.as_os_str().as_encoded_bytes(),
            &nanos.to_le_bytes(),
            &length.to_le_bytes(),
            &side.to_le_bytes(),
            &FORMAT.to_le_bytes(),
        ] {
            for &byte in bytes {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
        format!("{hash:0width$x}{EXTENSION}", width = NAME_HEX_DIGITS)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.dir.join(name).is_file()
    }

    /// A missing, oversized, undecodable, or wrongly sized file reads as a miss and is rebuilt.
    pub fn load(&self, name: &str, side: u32) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
        let path = self.dir.join(name);
        if std::fs::metadata(&path).ok()?.len() > MAX_STORED_BYTES {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        // Reject other dimensions from the header, before the decoder allocates pixels.
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(side);
        limits.max_image_height = Some(side);
        limits.max_alloc = Some(u64::from(side) * u64::from(side) * 4);
        let mut reader = ImageReader::with_format(std::io::Cursor::new(bytes), ImageFormat::Jpeg);
        reader.limits(limits);
        let decoder = reader.into_decoder().ok()?;
        if decoder.dimensions() != (side, side) {
            return None;
        }
        let image = DynamicImage::from_decoder(decoder).ok()?.into_rgb8();
        Some(SharedPixelBuffer::clone_from_slice(
            image.as_raw(),
            side,
            side,
        ))
    }

    /// Writes through a temporary file and a rename, so readers never see a partial thumbnail.
    pub fn save(&self, name: &str, image: &SharedPixelBuffer<Rgb8Pixel>) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let temp = self.dir.join(format!("{name}.{}.tmp", std::process::id()));
        let result = (|| {
            let mut file = std::io::BufWriter::new(std::fs::File::create(&temp)?);
            JpegEncoder::new_with_quality(&mut file, QUALITY)
                .encode(
                    image.as_bytes(),
                    image.width(),
                    image.height(),
                    ExtendedColorType::Rgb8,
                )
                .map_err(std::io::Error::other)?;
            file.flush()?;
            file.get_ref().sync_all()?;
            std::fs::rename(&temp, self.dir.join(name))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    /// Removes thumbnails and leftover temporary files not named in `keep`.
    /// Files this store did not name are left alone.
    pub fn prune(&self, keep: &HashSet<String>) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let stem = name
                .get(..NAME_HEX_DIGITS + EXTENSION.len())
                .unwrap_or(name);
            let ours = stem.len() == NAME_HEX_DIGITS + EXTENSION.len()
                && stem.ends_with(EXTENSION)
                && stem[..NAME_HEX_DIGITS]
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                && (name == stem || name.ends_with(".tmp"));
            if ours && (name != stem || !keep.contains(name)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ThumbStore;
    use slint::{Rgb8Pixel, SharedPixelBuffer};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(
                std::env::temp_dir()
                    .join(format!("photoframe-thumbs-{}-{unique}", std::process::id())),
            )
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn gradient(side: u32) -> SharedPixelBuffer<Rgb8Pixel> {
        let mut buffer = SharedPixelBuffer::<Rgb8Pixel>::new(side, side);
        for (i, pixel) in buffer.make_mut_slice().iter_mut().enumerate() {
            let (x, y) = (i as u32 % side, i as u32 / side);
            *pixel = Rgb8Pixel {
                r: (x * 255 / side) as u8,
                g: (y * 255 / side) as u8,
                b: 90,
            };
        }
        buffer
    }

    #[test]
    fn saved_thumbnail_round_trips_at_its_size() {
        let dir = TempDir::new();
        let store = ThumbStore::new(dir.0.join("nested"));
        let name =
            ThumbStore::file_name(Path::new("/photos/a.jpg"), SystemTime::UNIX_EPOCH, 10, 64);
        assert!(!store.contains(&name));
        let image = gradient(64);
        store.save(&name, &image).unwrap();
        assert!(store.contains(&name));
        let loaded = store.load(&name, 64).unwrap();
        assert_eq!((loaded.width(), loaded.height()), (64, 64));
        let error = image
            .as_bytes()
            .iter()
            .zip(loaded.as_bytes())
            .map(|(a, b)| a.abs_diff(*b) as u32)
            .max()
            .unwrap();
        assert!(error < 24, "JPEG error {error}");
        assert!(store.load(&name, 32).is_none());
        assert_eq!(std::fs::read_dir(&store.dir).unwrap().count(), 1);
    }

    #[test]
    fn name_is_stable_and_changes_with_source_version_or_size() {
        let path = Path::new("/photos/a.jpg");
        let time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let name = ThumbStore::file_name(path, time, 10, 270);
        assert_eq!(name, ThumbStore::file_name(path, time, 10, 270));
        assert_eq!(name.len(), 20);
        assert_ne!(
            name,
            ThumbStore::file_name(path, time + Duration::from_nanos(1), 10, 270)
        );
        assert_ne!(name, ThumbStore::file_name(path, time, 11, 270));
        assert_ne!(name, ThumbStore::file_name(path, time, 10, 271));
        assert_ne!(
            name,
            ThumbStore::file_name(Path::new("/photos/b.jpg"), time, 10, 270)
        );
    }

    #[test]
    fn corrupt_file_reads_as_a_miss() {
        let dir = TempDir::new();
        let store = ThumbStore::new(dir.0.clone());
        let name =
            ThumbStore::file_name(Path::new("/photos/a.jpg"), SystemTime::UNIX_EPOCH, 10, 64);
        std::fs::create_dir_all(&dir.0).unwrap();
        std::fs::write(dir.0.join(&name), b"not a jpeg").unwrap();
        assert!(store.load(&name, 64).is_none());
    }

    #[test]
    fn prune_removes_only_unlisted_store_files() {
        let dir = TempDir::new();
        let store = ThumbStore::new(dir.0.clone());
        let keep = ThumbStore::file_name(Path::new("/photos/a.jpg"), SystemTime::UNIX_EPOCH, 1, 64);
        let stale =
            ThumbStore::file_name(Path::new("/photos/b.jpg"), SystemTime::UNIX_EPOCH, 1, 64);
        for name in [&keep, &stale] {
            store.save(name, &gradient(8)).unwrap();
        }
        std::fs::write(dir.0.join(format!("{keep}.42.tmp")), b"partial").unwrap();
        std::fs::write(dir.0.join("notes.txt"), b"keep me").unwrap();
        std::fs::write(dir.0.join("holiday.jpg"), b"keep me").unwrap();
        store.prune(&HashSet::from([keep.clone()]));
        let mut left: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut expected = vec![keep, "holiday.jpg".to_owned(), "notes.txt".to_owned()];
        expected.sort();
        assert_eq!(left, expected);
    }
}
