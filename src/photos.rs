use frame_trace::Field;
use image::imageops::{self, FilterType};
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageReader, RgbImage, RgbaImage};
use slint::{ComponentHandle, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Cursor, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use crate::photo_load_state::{PhotoLoadState, PhotoSize};
use crate::{AppWindow, Frame, Photo, Saver};

mod cache_writer;
mod prewarm;
mod resample;
mod thumb_store;
pub(crate) mod trace;
use thumb_store::ThumbStore;

const SCRIM_SVG: &[u8] = include_bytes!("../ui/images/photo-scrim.svg");

#[derive(Clone, Debug, PartialEq, Eq)]
struct ThumbnailKey {
    path: PathBuf,
    modified: SystemTime,
    length: u64,
    size: PhotoSize,
    baked: bool,
}

impl ThumbnailKey {
    fn stored_name(&self) -> String {
        ThumbStore::file_name(&self.path, self.modified, self.length, self.size.0)
    }

    fn stored_full_name(&self) -> String {
        ThumbStore::full_name(
            &self.path,
            self.modified,
            self.length,
            self.size.0,
            self.size.1,
        )
    }

    fn read(path: &Path, size: PhotoSize, baked: bool) -> std::io::Result<Self> {
        let path = path.canonicalize()?;
        let metadata = path.metadata()?;
        Ok(Self {
            path,
            modified: metadata.modified()?,
            length: metadata.len(),
            size,
            baked,
        })
    }

    fn supersedes(&self, old: &Self) -> bool {
        self == old
            || (self.path == old.path
                && (self.modified != old.modified || self.length != old.length))
    }
}

struct ThumbnailCache {
    entries: VecDeque<(ThumbnailKey, SharedPixelBuffer<Rgb8Pixel>)>,
    bytes: usize,
    max_entries: usize,
    max_bytes: usize,
}

impl ThumbnailCache {
    const fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            max_entries,
            max_bytes,
        }
    }

    fn get(&mut self, key: &ThumbnailKey) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
        let index = self
            .entries
            .iter()
            .position(|(candidate, _)| candidate == key)?;
        let entry = self.entries.remove(index)?;
        let image = entry.1.clone();
        self.entries.push_back(entry);
        Some(image)
    }

    fn insert(&mut self, key: ThumbnailKey, image: SharedPixelBuffer<Rgb8Pixel>) {
        self.entries.retain(|(old, _)| !key.supersedes(old));
        self.bytes = self
            .entries
            .iter()
            .map(|(_, image)| image.as_bytes().len())
            .sum();
        let size = image.as_bytes().len();
        if size > self.max_bytes {
            return;
        }
        while self.bytes + size > self.max_bytes || self.entries.len() >= self.max_entries {
            if let Some((_, old)) = self.entries.pop_front() {
                self.bytes -= old.as_bytes().len();
            }
        }
        self.bytes += size;
        self.entries.push_back((key, image));
    }
}

static RENDITIONS: Mutex<ThumbnailCache> = Mutex::new(ThumbnailCache::new(16, 16 * 1024 * 1024));
// Grid thumbnails get their own cache so card and full-screen renditions cannot evict them.
static GRID_THUMBNAILS: Mutex<ThumbnailCache> =
    Mutex::new(ThumbnailCache::new(128, 28 * 1024 * 1024));

fn cache_for(kind: Kind) -> &'static Mutex<ThumbnailCache> {
    if kind == Kind::Thumb {
        &GRID_THUMBNAILS
    } else {
        &RENDITIONS
    }
}

fn raster_scrim(w: u32, h: u32) -> Result<RgbaImage, &'static str> {
    let image =
        slint::Image::load_from_svg_data(SCRIM_SVG).map_err(|_| "cannot load caption SVG")?;
    let buffer = image.to_rgba8().ok_or("cannot rasterize caption SVG")?;
    let rgba = RgbaImage::from_raw(buffer.width(), buffer.height(), buffer.as_bytes().to_vec())
        .ok_or("invalid caption SVG raster")?;
    Ok(imageops::resize(&rgba, w, h, FilterType::Triangle))
}

fn composite_scrim(photo: &mut SharedPixelBuffer<Rgb8Pixel>, scrim: &RgbaImage) {
    let start = (photo.height() - scrim.height()) as usize * photo.width() as usize;
    for (background, foreground) in photo.make_mut_slice()[start..]
        .iter_mut()
        .zip(scrim.pixels())
    {
        let alpha = foreground[3] as u32;
        let blend = |color: u8, backdrop: u8| {
            ((color as u32 * alpha + backdrop as u32 * (255 - alpha) + 127) / 255) as u8
        };
        background.r = blend(foreground[0], background.r);
        background.g = blend(foreground[1], background.g);
        background.b = blend(foreground[2], background.b);
    }
}

const THUMB: u32 = 270;

struct Entry {
    path: PathBuf,
    caption: String,
}

static PLAYLIST: OnceLock<Vec<Entry>> = OnceLock::new();
static STORE: OnceLock<Option<cache_writer::CacheStore>> = OnceLock::new();

fn store() -> Option<&'static cache_writer::CacheStore> {
    STORE
        .get_or_init(cache_writer::CacheStore::from_env)
        .as_ref()
}

pub fn count() -> usize {
    PLAYLIST.get().map_or(0, Vec::len)
}

fn bundled_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|parent| parent.join("assets/photos")))
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/photos"))
}

fn dir() -> PathBuf {
    std::env::var_os("FRAME_PHOTOS")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(bundled_dir)
}

fn exif_date(exif: &[u8]) -> Option<String> {
    let tiff = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let value_of = |ifd: usize, tag: u16| -> Option<usize> {
        (0..u16_at(ifd)? as usize).find_map(|i| {
            let entry = ifd.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
            (u16_at(entry) == Some(tag))
                .then(|| entry.checked_add(8))
                .flatten()
        })
    };
    let exif_ifd = u32_at(value_of(u32_at(4)? as usize, 0x8769)?)? as usize;
    let text = u32_at(value_of(exif_ifd, 0x9003)?)? as usize;
    std::str::from_utf8(tiff.get(text..text.checked_add(19)?)?)
        .ok()
        .map(str::to_owned)
}

// Reads markers up to the first scan, so the playlist scan costs a few kilobytes per photo
// instead of the whole file.
fn exif_segment(path: &Path) -> Option<Vec<u8>> {
    let mut file = std::io::BufReader::new(std::fs::File::open(path).ok()?);
    let mut byte = [0u8; 2];
    file.read_exact(&mut byte).ok()?;
    if byte != [0xff, 0xd8] {
        return None;
    }
    loop {
        file.read_exact(&mut byte[..1]).ok()?;
        if byte[0] != 0xff {
            return None;
        }
        // Any number of 0xFF fill bytes may precede the marker code.
        while byte[0] == 0xff {
            file.read_exact(&mut byte[..1]).ok()?;
        }
        let marker = byte[0];
        match marker {
            0xd9 | 0xda | 0x00 => return None,
            0x01 | 0xd0..=0xd7 => continue,
            _ => {}
        }
        file.read_exact(&mut byte).ok()?;
        let length = usize::from(u16::from_be_bytes(byte)).checked_sub(2)?;
        let mut segment = vec![0; length];
        file.read_exact(&mut segment).ok()?;
        if marker == 0xe1 && segment.starts_with(b"Exif\0\0") {
            return Some(segment);
        }
    }
}

fn taken(path: &Path) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(&exif_date(&exif_segment(path)?)?, "%Y:%m:%d %H:%M:%S")
        .ok()
}

fn scan(dir: &Path) -> Vec<Entry> {
    let mut photos: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg"))
        })
        .map(|path| (taken(&path), path))
        .collect();
    photos.sort();
    photos
        .into_iter()
        .map(|(date, path)| Entry {
            caption: date.map_or_else(String::new, |d| d.format("%B %Y").to_string()),
            path,
        })
        .collect()
}

pub fn init(app: &AppWindow) {
    trace::install_ui(app);
    let directory = dir();
    let bundled = bundled_dir();
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let scan_trace = frame_trace::enabled().then(|| (frame_trace::next_id(), Instant::now()));
        let mut entries = scan(&directory);
        let fallback = entries.is_empty() && directory != bundled;
        if fallback {
            entries = scan(&bundled);
        }
        let count = entries.len();
        if PLAYLIST.set(entries).is_err() {
            return;
        }
        if let Some((id, started)) = scan_trace {
            frame_trace::event(
                "photo.playlist.scan",
                Some(id),
                &[
                    Field::U64("count", count as u64),
                    Field::Bool("fallback", fallback),
                    Field::U64("elapsed_us", trace::micros(started)),
                ],
            );
        }
        let (published, publication) = std::sync::mpsc::sync_channel(1);
        let ui = weak.clone();
        let publish_queued = scan_trace
            .filter(|_| frame_trace::enabled())
            .map(|_| Instant::now());
        if slint::invoke_from_event_loop(move || {
            let Some(app) = ui.upgrade() else { return };
            publish_playlist(&app);
            if let (Some((id, _)), Some(queued)) = (scan_trace, publish_queued) {
                frame_trace::event(
                    "photo.playlist.publish",
                    Some(id),
                    &[
                        Field::U64("count", count as u64),
                        Field::U64("enqueue_to_publish_us", trace::micros(queued)),
                    ],
                );
            }
            let _ = published.send(());
        })
        .is_err()
            || publication.recv().is_err()
        {
            return;
        }
        if store().is_none() {
            SHARED.lock().unwrap().warm.next = PLAYLIST.get().map_or(0, Vec::len);
        }
        for _ in 1..decode_threads() {
            let weak = weak.clone();
            std::thread::spawn(move || work(weak));
        }
        work(weak);
    });
}

fn publish_playlist(app: &AppWindow) {
    let rows: Vec<Photo> = PLAYLIST
        .get()
        .unwrap()
        .iter()
        .map(|e| Photo {
            title: e.caption.as_str().into(),
            ..Default::default()
        })
        .collect();
    let frame = app.global::<Frame>();
    frame.set_photos(ModelRc::new(VecModel::from(rows)));
    frame.set_photos_loading(false);
    demand(app);
}

const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RESIZE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_THUMBS: usize = 64;

fn memory_limit() -> image::ImageError {
    image::ImageError::Limits(image::error::LimitError::from_kind(
        image::error::LimitErrorKind::InsufficientMemory,
    ))
}

fn read_photo(path: &Path) -> image::ImageResult<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_FILE_BYTES {
        return Err(memory_limit());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(memory_limit());
    }
    Ok(bytes)
}

fn photo_decoder(bytes: &[u8]) -> image::ImageResult<impl ImageDecoder + '_> {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_SOURCE_BYTES);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Jpeg);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    if decoder.total_bytes() > MAX_SOURCE_BYTES
        || u64::from(width) * u64::from(height) * 3 > MAX_SOURCE_BYTES
    {
        return Err(memory_limit());
    }
    Ok(decoder)
}

/// Output length of a JPEG axis decoded at `idct`/8 scale.
fn scaled_len(len: u32, idct: u32) -> u32 {
    (len * idct).div_ceil(8)
}

/// The smallest DCT scale (in eighths) whose output still covers `target` the way `fit`
/// does. 8 means a full decode, which is also the answer whenever `fit` would not shrink.
fn dct_scale(oriented: (u32, u32), target: (u32, u32)) -> u32 {
    let (w, h) = oriented;
    let scale = (target.0 as f32 / w as f32).max(target.1 as f32 / h as f32);
    if scale >= 1.0 {
        return 8;
    }
    let (cover_w, cover_h) = (
        (w as f32 * scale).ceil() as u32,
        (h as f32 * scale).ceil() as u32,
    );
    [1, 2, 4]
        .into_iter()
        .find(|&idct| scaled_len(w, idct) >= cover_w && scaled_len(h, idct) >= cover_h)
        .unwrap_or(8)
}

fn oriented_dimensions((w, h): (u32, u32), orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (h, w),
        _ => (w, h),
    }
}

/// `None` when jpeg-decoder cannot produce RGB at this scale; the caller decodes in full.
fn decode_scaled(bytes: &[u8], (w, h): (u32, u32), idct: u32) -> Option<RgbImage> {
    let (sw, sh) = (scaled_len(w, idct), scaled_len(h, idct));
    let mut decoder = jpeg_decoder::Decoder::new(Cursor::new(bytes));
    decoder.set_max_decoding_buffer_size(MAX_SOURCE_BYTES as usize);
    let size = decoder
        .scale(u16::try_from(sw).ok()?, u16::try_from(sh).ok()?)
        .ok()?;
    if u32::from(size.0) != sw || u32::from(size.1) != sh {
        return None;
    }
    let pixels = decoder.decode().ok()?;
    let rgb = match decoder.info()?.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => pixels,
        jpeg_decoder::PixelFormat::L8 => pixels.into_iter().flat_map(|l| [l; 3]).collect(),
        _ => return None,
    };
    RgbImage::from_raw(sw, sh, rgb)
}

struct Decoded {
    image: RgbImage,
    oriented: (u32, u32),
    idct: u32,
}

impl Decoded {
    fn covers(&self, target: (u32, u32)) -> bool {
        self.idct >= dct_scale(self.oriented, target)
    }
}

/// Decodes at the smallest DCT scale that still covers `target` after orientation.
fn decode_oriented(
    path: &Path,
    target: (u32, u32),
    context: Option<trace::Context>,
) -> image::ImageResult<Decoded> {
    let started = trace::start(context);
    let bytes = read_photo(path);
    match &bytes {
        Ok(bytes) => trace::duration(
            context,
            "photo.original.read",
            started,
            &[
                Field::Bool("ok", true),
                Field::U64("bytes", bytes.len() as u64),
            ],
        ),
        Err(_) => trace::duration(
            context,
            "photo.original.read",
            started,
            &[Field::Bool("ok", false)],
        ),
    }
    let bytes = bytes?;
    let started = trace::start(context);
    let result = (|| {
        let mut decoder = photo_decoder(&bytes)?;
        let orientation = decoder.orientation()?;
        let source = decoder.dimensions();
        let oriented = oriented_dimensions(source, orientation);
        let idct = dct_scale(oriented, target);
        let scaled = (idct < 8)
            .then(|| decode_scaled(&bytes, source, idct))
            .flatten();
        let (mut image, idct) = match scaled {
            Some(rgb) => (DynamicImage::ImageRgb8(rgb), idct),
            None => (DynamicImage::from_decoder(decoder)?, 8),
        };
        image.apply_orientation(orientation);
        Ok(Decoded {
            image: image.into_rgb8(),
            oriented,
            idct,
        })
    })();
    if let Ok(decoded) = &result {
        trace::duration(
            context,
            "photo.original.decode",
            started,
            &[
                Field::Bool("ok", true),
                Field::U64("width", u64::from(decoded.image.width())),
                Field::U64("height", u64::from(decoded.image.height())),
                Field::U64("idct_eighths", u64::from(decoded.idct)),
            ],
        );
    } else {
        trace::duration(
            context,
            "photo.original.decode",
            started,
            &[Field::Bool("ok", false)],
        );
    }
    result
}

#[cfg(test)]
fn cover(
    path: &Path,
    w: u32,
    h: u32,
    current: impl Fn() -> bool,
) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
    if w == 0 || h == 0 || !current() {
        return None;
    }
    let decoded = decode_oriented(path, (w, h), None)
        .map_err(|e| eprintln!("[photos] {}: {e}", path.display()))
        .ok()?;
    fit(&decoded.image, w, h, current)
}

fn fit(
    img: &RgbImage,
    w: u32,
    h: u32,
    current: impl Fn() -> bool,
) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
    if w == 0
        || h == 0
        || w > 4096
        || h > 4096
        || u64::from(w) * u64::from(h) * 3 > MAX_OUTPUT_BYTES
        || !current()
    {
        return None;
    }
    let scale = (w as f32 / img.width() as f32).max(h as f32 / img.height() as f32);
    let (sw, sh) = (
        (img.width() as f32 * scale).ceil() as u32,
        (img.height() as f32 * scale).ceil() as u32,
    );
    if u64::from(sw) * u64::from(sh) * 3 > MAX_RESIZE_BYTES {
        eprintln!("[photos] cover resize exceeds the pixel budget");
        return None;
    }
    let mut output = SharedPixelBuffer::<Rgb8Pixel>::new(w, h);
    resample::cover_into(img, (sw.max(w), sh.max(h)), (w, h), output.make_mut_bytes());
    if !current() {
        return None;
    }
    Some(output)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    Card,
    Full,
    Thumb,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Card => "card",
            Self::Full => "full",
            Self::Thumb => "thumb",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Card {
    size: PhotoSize,
    bake: bool,
    generation: u64,
}

#[derive(Default, PartialEq)]
struct Want {
    current: usize,
    card_displayed: Vec<usize>,
    full_displayed: Vec<usize>,
    card: Option<Card>,
    screen: Option<(u32, u32)>,
    full: bool,
    thumbs: Range<usize>,
}

#[derive(Clone, Copy)]
enum WindowUrgency {
    Current,
    Next,
    JustLeft,
    Prefetch,
}

impl WindowUrgency {
    const ORDER: [Self; 4] = [Self::Current, Self::Next, Self::JustLeft, Self::Prefetch];

    fn offset(self, count: usize) -> usize {
        match self {
            Self::Current => 0,
            Self::Next => 1,
            Self::JustLeft => count.saturating_sub(1),
            Self::Prefetch => 2,
        }
    }
}

fn window(current: usize, count: usize) -> Vec<usize> {
    let span = count.max(1);
    let mut out = Vec::with_capacity(WindowUrgency::ORDER.len());
    for urgency in WindowUrgency::ORDER {
        let index = (current % span + urgency.offset(count)) % span;
        if index < count && !out.contains(&index) {
            out.push(index);
        }
    }
    out
}

impl Want {
    fn slots(&self, count: usize) -> Vec<(usize, Kind, PhotoSize)> {
        let mut card_slots = Vec::new();
        let mut card_indexes = self.card_displayed.clone();
        if !self.full {
            for index in window(self.current, count) {
                if !card_indexes.contains(&index) {
                    card_indexes.push(index);
                }
            }
        }
        for index in card_indexes.into_iter().filter(|&index| index < count) {
            if let Some(Card {
                size: (w, h, scrim),
                bake,
                ..
            }) = self.card
            {
                card_slots.push((index, Kind::Card, (w, h, if bake { scrim } else { 0 })));
            }
        }
        let mut screen_slots = Vec::new();
        let mut screen_indexes = vec![self.current];
        for &index in &self.full_displayed {
            if !screen_indexes.contains(&index) {
                screen_indexes.push(index);
            }
        }
        for index in window(self.current, count) {
            if !screen_indexes.contains(&index) {
                screen_indexes.push(index);
            }
        }
        if self.full {
            for index in screen_indexes.into_iter().filter(|&index| index < count) {
                if let Some((w, h)) = self.screen {
                    screen_slots.push((index, Kind::Full, (w, h, 0)));
                }
            }
        }
        let mut slots = if self.full {
            screen_slots.extend(card_slots);
            screen_slots
        } else {
            card_slots.extend(screen_slots);
            card_slots
        };
        slots.extend(
            self.thumbs
                .clone()
                .filter(|&i| i < count)
                .take(MAX_THUMBS)
                .map(|i| (i, Kind::Thumb, (THUMB, THUMB, 0))),
        );
        slots
    }
}

struct Job {
    trace: Option<trace::Context>,
    index: usize,
    specs: Vec<(Kind, PhotoSize)>,
}

struct Step {
    evict: Vec<(usize, Kind)>,
    job: Option<Job>,
    card_done: Option<Card>,
}

#[derive(Clone, Copy, PartialEq)]
enum HaveSlot {
    Ready(PhotoSize),
    Failed(PhotoSize),
}

impl HaveSlot {
    fn size(self) -> PhotoSize {
        match self {
            HaveSlot::Ready(size) | HaveSlot::Failed(size) => size,
        }
    }
}

#[derive(PartialEq)]
enum CardBatch {
    Open,
    Settled,
}

/// Idle thumbnail pass shared by all workers: photos not yet handed out, photos handed back
/// because display work preempted them, and how many are stored.
struct Warm {
    next: usize,
    retry: Vec<usize>,
    done: usize,
}

impl Warm {
    const fn new() -> Self {
        Self {
            next: 0,
            retry: Vec::new(),
            done: 0,
        }
    }

    fn claim(&mut self, count: usize) -> Option<usize> {
        self.retry.pop().or_else(|| {
            let index = self.next;
            (index < count).then(|| {
                self.next += 1;
                index
            })
        })
    }
}

struct Shared {
    want: Want,
    have: BTreeMap<(usize, Kind), HaveSlot>,
    card_batch: CardBatch,
    // Photo indexes a worker is rendering or publishing. A claimed index is never offered again.
    in_flight: Vec<usize>,
    warm: Warm,
    trace_demand: Option<(u64, Instant)>,
}

type Buffer = SharedPixelBuffer<Rgb8Pixel>;

enum Rendered {
    Ready(Buffer),
    Failed,
    // Demand dropped the slot mid-render. Never recorded, so the slot reloads if wanted again.
    Cancelled,
}

impl Shared {
    const fn new() -> Self {
        Self {
            want: Want {
                current: 0,
                card_displayed: Vec::new(),
                full_displayed: Vec::new(),
                card: None,
                screen: None,
                full: false,
                thumbs: 0..0,
            },
            have: BTreeMap::new(),
            card_batch: CardBatch::Settled,
            in_flight: Vec::new(),
            warm: Warm::new(),
            trace_demand: None,
        }
    }

    // Slot membership alone decides: scrolling changes demand every row, and a decode that is
    // still wanted must finish rather than restart.
    fn accepts(&self, index: usize, kind: Kind, spec: PhotoSize, count: usize) -> bool {
        self.want.slots(count).contains(&(index, kind, spec))
    }

    fn filled(&self, index: usize, kind: Kind, spec: PhotoSize) -> bool {
        self.have
            .get(&(index, kind))
            .map_or(false, |slot| slot.size() == spec)
    }

    fn step(&mut self, count: usize) -> Step {
        let slots = self.want.slots(count);
        let evict: Vec<_> = self
            .have
            .keys()
            .copied()
            .filter(|&(i, k)| !slots.iter().any(|&(si, sk, _)| (si, sk) == (i, k)))
            .collect();
        let missing: Vec<_> = slots
            .iter()
            .filter(|&&(i, k, spec)| !self.filled(i, k, spec))
            .collect();
        let job = missing
            .iter()
            .find(|s| !self.in_flight.contains(&s.0))
            .map(|&&(index, ..)| Job {
                trace: None,
                index,
                specs: missing
                    .iter()
                    .filter(|s| s.0 == index)
                    .map(|&&(_, k, spec)| (k, spec))
                    .collect(),
            });
        let card_missing = missing.iter().any(|s| s.1 == Kind::Card);
        let card_done = if self.card_batch == CardBatch::Open && !card_missing {
            self.want.card
        } else {
            None
        };
        Step {
            evict,
            job,
            card_done,
        }
    }
}

impl Shared {
    /// Picks the next step and takes its job's photo and a finished card batch, so no other
    /// worker repeats them. Every job needs a matching `release`.
    fn claim(&mut self, count: usize) -> Step {
        let mut step = self.step(count);
        if let Some(job) = &mut step.job {
            self.in_flight.push(job.index);
            job.trace = trace::Context::new(job.index, false);
            if let (Some(_), Some((demand_id, queued))) = (job.trace, self.trace_demand) {
                trace::emit(
                    job.trace,
                    "photo.job.claim",
                    &[
                        Field::U64("demand_id", demand_id),
                        Field::U64("demand_age_us", trace::micros(queued)),
                        Field::U64("renditions", job.specs.len() as u64),
                    ],
                );
            } else {
                trace::emit(
                    job.trace,
                    "photo.job.claim",
                    &[Field::U64("renditions", job.specs.len() as u64)],
                );
            }
        }
        if step.card_done.is_some() {
            self.card_batch = CardBatch::Settled;
        }
        step
    }

    fn release(&mut self, index: usize) {
        self.in_flight.retain(|&claimed| claimed != index);
    }
}

fn threads_from(value: Option<&str>) -> usize {
    value.and_then(|v| v.parse().ok()).unwrap_or(2).clamp(1, 3)
}

fn decode_threads() -> usize {
    threads_from(std::env::var("FRAME_DECODE_THREADS").ok().as_deref())
}

static SHARED: Mutex<Shared> = Mutex::new(Shared::new());
static DEMAND_CHANGED: Condvar = Condvar::new();
static LOAD_STATE: Mutex<PhotoLoadState> = Mutex::new(PhotoLoadState::new());

fn update(change: impl FnOnce(&mut Shared) -> bool) {
    let mut shared = SHARED.lock().unwrap();
    if change(&mut shared) {
        shared.trace_demand =
            frame_trace::enabled().then(|| (frame_trace::next_id(), Instant::now()));
        if let Some((id, _)) = shared.trace_demand {
            frame_trace::event(
                "photo.demand",
                Some(id),
                &[
                    Field::U64("source_index", shared.want.current as u64),
                    Field::Bool("full", shared.want.full),
                    Field::U64("grid_first", shared.want.thumbs.start as u64),
                    Field::U64("grid_end", shared.want.thumbs.end as u64),
                ],
            );
        }
        DEMAND_CHANGED.notify_all();
    }
}

pub fn load_scaled(size: PhotoSize, bake: bool) {
    let request = LOAD_STATE.lock().unwrap().request_mode(size, bake);
    let generation = request.generation();
    let started = request.started();
    update(|s| {
        let card = Some(Card {
            size,
            bake,
            generation,
        });
        if !started && s.want.card == card {
            return false;
        }
        s.want.card = card;
        s.card_batch = CardBatch::Open;
        true
    });
}

pub fn demand(app: &AppWindow) {
    let f = app.global::<Frame>();
    let saver = app.global::<Saver>();
    let rows = |v: i32| v.max(0) as usize;
    update(|s| {
        // Off the Photos tab, the last grid range stays resident so returning shows it at once.
        let thumbs = if f.get_active_tab() == 2 {
            rows(saver.get_grid_first())..rows(saver.get_grid_end())
        } else {
            s.want.thumbs.clone()
        };
        let pair = |current: i32, previous: i32| {
            [current, previous]
                .into_iter()
                .filter(|&index| index >= 0)
                .map(|index| index as usize)
                .collect()
        };
        let (w, h) = (
            saver.get_pixel_width().max(0) as u32,
            saver.get_pixel_height().max(0) as u32,
        );
        let want = Want {
            current: rows(f.get_photo_index()),
            card_displayed: pair(saver.get_card_current(), saver.get_card_previous()),
            full_displayed: pair(saver.get_full_current(), saver.get_full_previous()),
            card: s.want.card,
            screen: (w > 0 && h > 0).then_some((w, h)),
            full: saver.get_active(),
            thumbs,
        };
        if s.want == want {
            return false;
        }
        s.want = want;
        true
    });
    prewarm::refresh(app);
}

/// Reuses `slot` when it already covers `target`; otherwise frees it before decoding again
/// so two full-size buffers never coexist. A failed decode is remembered for the job.
fn decode_for<'a>(
    slot: &'a mut Option<Option<Decoded>>,
    path: &Path,
    target: (u32, u32),
    context: Option<trace::Context>,
) -> Option<&'a RgbImage> {
    let reusable = match slot {
        Some(Some(decoded)) => decoded.covers(target),
        Some(None) => true,
        None => false,
    };
    trace::emit(
        context,
        "photo.original.reuse",
        &[Field::Str(
            "result",
            match slot {
                Some(Some(_)) if reusable => "hit",
                Some(None) => "failed",
                _ => "miss",
            },
        )],
    );
    if !reusable {
        *slot = None;
        *slot = Some(
            decode_oriented(path, target, context)
                .map_err(|e| eprintln!("[photos] {}: {e}", path.display()))
                .ok(),
        );
    }
    slot.as_ref()?.as_ref().map(|decoded| &decoded.image)
}

struct PendingFull {
    trace: Option<trace::Context>,
    key: ThumbnailKey,
    buffer: Buffer,
}

fn render(
    path: &Path,
    job: &Job,
    wanted: impl Fn(Kind, PhotoSize) -> bool,
) -> (Vec<(Kind, PhotoSize, Rendered)>, Option<PendingFull>) {
    render_with_store(path, job, wanted, store().map(|store| &*store.disk))
}

fn render_with_store(
    path: &Path,
    job: &Job,
    wanted: impl Fn(Kind, PhotoSize) -> bool,
    store: Option<&ThumbStore>,
) -> (Vec<(Kind, PhotoSize, Rendered)>, Option<PendingFull>) {
    let mut decoded = None;
    let mut out = Vec::new();
    let mut pending = None;
    // Largest first, so one decode serves every rendition of the photo.
    let mut specs: Vec<_> = job.specs.iter().collect();
    specs.sort_by_key(|(_, (w, h, _))| std::cmp::Reverse(u64::from(*w) * u64::from(*h)));
    for &(kind, spec @ (w, h, scrim)) in specs {
        let context = job.trace.map(|context| context.rendition(kind.label()));
        let rendition_started = trace::start(context);
        if !wanted(kind, spec) {
            trace::emit(
                context,
                "photo.rendition.cancel",
                &[Field::Str("stage", "before_render")],
            );
            continue;
        }
        trace::emit(
            context,
            "photo.rendition.begin",
            &[
                Field::U64("width", u64::from(w)),
                Field::U64("height", u64::from(h)),
                Field::U64("scrim_height", u64::from(scrim)),
            ],
        );
        let key = ThumbnailKey::read(path, spec, scrim > 0).ok();
        let stored = key.as_ref().and_then(|key| {
            let store = store?;
            match kind {
                Kind::Thumb => Some((store, key.stored_name())),
                Kind::Full => Some((store, key.stored_full_name())),
                _ => None,
            }
        });
        let cache_started = trace::start(context);
        let cached = key
            .as_ref()
            .and_then(|key| cache_for(kind).lock().unwrap().get(key));
        trace::duration(
            context,
            "photo.cache.ram",
            cache_started,
            &[
                Field::Bool("hit", cached.is_some()),
                Field::Bool("key_valid", key.is_some()),
            ],
        );
        let cached = cached.or_else(|| {
            let Some((store, name)) = stored.as_ref() else {
                trace::emit(
                    context,
                    "photo.cache.disk",
                    &[Field::Str("result", "unavailable")],
                );
                return None;
            };
            let started = trace::start(context);
            let buf = if kind == Kind::Full {
                store.load_full(name, w, h)
            } else {
                store.load(name, w)
            };
            trace::duration(
                context,
                "photo.cache.disk",
                started,
                &[Field::Str(
                    "result",
                    if buf.is_some() { "hit" } else { "miss" },
                )],
            );
            let buf = buf?;
            if let Some(key) = &key {
                cache_for(kind)
                    .lock()
                    .unwrap()
                    .insert(key.clone(), buf.clone());
            }
            Some(buf)
        });
        let rendered = cached
            .or_else(|| {
                let image = decode_for(&mut decoded, path, (w, h), context)?;
                // After the full decode, a thumbnail is worth finishing and storing even if
                // its tile scrolled away; the cost is already paid.
                let started = trace::start(context);
                let buf = fit(image, w, h, || kind == Kind::Thumb || wanted(kind, spec));
                trace::duration(
                    context,
                    "photo.fit",
                    started,
                    &[Field::Bool("ok", buf.is_some())],
                );
                let mut buf = buf?;
                if scrim > 0 {
                    let started = trace::start(context);
                    match raster_scrim(w, scrim.min(h)) {
                        Ok(scrim) => {
                            composite_scrim(&mut buf, &scrim);
                            trace::duration(
                                context,
                                "photo.scrim",
                                started,
                                &[Field::Bool("ok", true)],
                            );
                        }
                        Err(error) => {
                            trace::duration(
                                context,
                                "photo.scrim",
                                started,
                                &[Field::Bool("ok", false)],
                            );
                            eprintln!("[photos] {error}; keeping the previous photo");
                            return None;
                        }
                    }
                }
                if let Some(key) = key {
                    if ThumbnailKey::read(path, spec, scrim > 0).ok().as_ref() == Some(&key) {
                        if let Some((store, name)) = &stored {
                            if kind == Kind::Full {
                                trace::emit(context, "photo.store.defer", &[]);
                                pending = Some(PendingFull {
                                    trace: context,
                                    key: key.clone(),
                                    buffer: buf.clone(),
                                });
                            } else {
                                save_stored(store, name, &buf, context);
                            }
                        }
                        cache_for(kind).lock().unwrap().insert(key, buf.clone());
                    }
                }
                Some(buf)
            })
            .map(Rendered::Ready)
            .unwrap_or_else(|| {
                if wanted(kind, spec) {
                    Rendered::Failed
                } else {
                    Rendered::Cancelled
                }
            });
        trace::duration(
            context,
            "photo.rendition.end",
            rendition_started,
            &[Field::Str(
                "result",
                match &rendered {
                    Rendered::Ready(_) => "ready",
                    Rendered::Failed => "failed",
                    Rendered::Cancelled => "cancelled",
                },
            )],
        );
        out.push((kind, spec, rendered));
    }
    (out, pending)
}

fn save_pending_full(store: &ThumbStore, pending: PendingFull) {
    let context = pending.trace;
    let key = pending.key;
    if ThumbnailKey::read(&key.path, key.size, key.baked)
        .ok()
        .as_ref()
        != Some(&key)
    {
        trace::emit(
            context,
            "photo.store.cancel",
            &[Field::Str("reason", "source_changed")],
        );
        return;
    }
    let name = key.stored_full_name();
    if let Err(error) = store.save_full(&name, &pending.buffer, context) {
        eprintln!("[photos] cannot store full-screen image {name}: {error}");
    }
}

fn save_stored(store: &ThumbStore, name: &str, buf: &Buffer, context: Option<trace::Context>) {
    if let Err(error) = store.save(name, buf, context) {
        eprintln!("[photos] cannot store thumbnail {name}: {error}");
    }
}

/// Builds one missing stored thumbnail without publishing it. Returns `false` when it
/// stopped early because `urgent` reported display work; the photo is retried later.
fn warm_stored(
    store: &ThumbStore,
    path: &Path,
    urgent: impl Fn() -> bool,
    context: Option<trace::Context>,
) -> bool {
    let Ok(key) = ThumbnailKey::read(path, (THUMB, THUMB, 0), false) else {
        return true;
    };
    let name = key.stored_name();
    let started = trace::start(context);
    let hit = store.contains(&name);
    trace::duration(
        context,
        "photo.cache.presence",
        started,
        &[Field::Bool("hit", hit)],
    );
    if hit {
        return true;
    }
    let Ok(decoded) = decode_oriented(path, (THUMB, THUMB), context)
        .map_err(|e| eprintln!("[photos] {}: {e}", path.display()))
    else {
        return true;
    };
    let started = trace::start(context);
    let buf = fit(&decoded.image, THUMB, THUMB, || !urgent());
    trace::duration(
        context,
        "photo.fit",
        started,
        &[Field::Bool("ok", buf.is_some())],
    );
    let Some(buf) = buf else {
        return !urgent();
    };
    if urgent() {
        return false;
    }
    save_stored(store, &name, &buf, context);
    true
}

fn prune_stored(store: &ThumbStore, playlist: &[Entry]) {
    let keep = playlist
        .iter()
        .filter_map(|entry| ThumbnailKey::read(&entry.path, (THUMB, THUMB, 0), false).ok())
        .map(|key| key.stored_name())
        .collect();
    store.prune(&keep);
}

fn work(weak: slint::Weak<AppWindow>) {
    let playlist = PLAYLIST.get().map_or(&[][..], Vec::as_slice);
    // While no demand is pending, store a thumbnail for every photo, one per pass, so
    // demand is re-checked between decodes. This runs once per new or changed photo.
    loop {
        let claimed = {
            let mut shared = SHARED.lock().unwrap();
            loop {
                let step = shared.claim(playlist.len());
                if step.job.is_some() || !step.evict.is_empty() || step.card_done.is_some() {
                    break Ok(step);
                }
                if let Some(index) = shared.warm.claim(playlist.len()) {
                    break Err(index);
                }
                shared = DEMAND_CHANGED.wait(shared).unwrap();
            }
        };
        let step = match claimed {
            Ok(step) => step,
            Err(index) => {
                let Some(store) = store() else { continue };
                let context = trace::Context::new(index, true).map(|c| c.rendition("thumb"));
                let started = trace::start(context);
                trace::emit(context, "photo.job.claim", &[]);
                let urgent = || SHARED.lock().unwrap().step(playlist.len()).job.is_some();
                let stored = warm_stored(&store.disk, &playlist[index].path, urgent, context);
                trace::duration(
                    context,
                    "photo.job.end",
                    started,
                    &[Field::Bool("retry", !stored)],
                );
                let mut shared = SHARED.lock().unwrap();
                if !stored {
                    shared.warm.retry.push(index);
                    DEMAND_CHANGED.notify_all();
                    continue;
                }
                shared.warm.done += 1;
                let finished = shared.warm.done == playlist.len();
                drop(shared);
                if finished {
                    let started = trace::start(context);
                    prune_stored(&store.disk, playlist);
                    trace::duration(context, "photo.store.prune", started, &[]);
                }
                continue;
            }
        };
        let claimed_index = step.job.as_ref().map(|job| job.index);
        let context = step.job.as_ref().and_then(|job| job.trace);
        let job_started = trace::start(context);
        let mut updates = Vec::new();
        let mut pending = None;
        if let Some(job) = &step.job {
            let (outcomes, full) = render(&playlist[job.index].path, job, |kind, spec| {
                SHARED
                    .lock()
                    .unwrap()
                    .accepts(job.index, kind, spec, playlist.len())
            });
            pending = full;
            for (kind, spec, rendered) in outcomes {
                updates.push((job.index, kind, spec, rendered));
            }
        }
        let (acknowledge, acknowledged) = std::sync::mpsc::sync_channel::<()>(1);
        let ui_enqueued = trace::start(context);
        trace::emit(
            context,
            "photo.ui.enqueue",
            &[Field::U64("updates", updates.len() as u64)],
        );
        if slint::invoke_from_event_loop({
            let weak = weak.clone();
            move || {
                let _acknowledge = acknowledge;
                trace::duration(context, "photo.ui.begin", ui_enqueued, &[]);
                let apply_started = trace::start(context);
                let Some(app) = weak.upgrade() else {
                    trace::emit(
                        context,
                        "photo.ui.reject",
                        &[Field::Str("reason", "app_gone")],
                    );
                    return;
                };
                demand(&app);
                let mut shared = SHARED.lock().unwrap();
                let card = shared.want.card;
                let slots = shared.want.slots(playlist.len());
                let mut admitted = Vec::new();
                for (index, kind) in step.evict {
                    if !slots.iter().any(|&(i, k, _)| (i, k) == (index, kind)) {
                        shared.have.remove(&(index, kind));
                        admitted.push((index, kind, None));
                    }
                }
                for (index, kind, spec, rendered) in updates {
                    let rendition_context = context.map(|c| c.rendition(kind.label()));
                    if slots.contains(&(index, kind, spec)) {
                        match rendered {
                            Rendered::Ready(buffer) => {
                                shared.have.insert((index, kind), HaveSlot::Ready(spec));
                                admitted.push((index, kind, Some(buffer)));
                            }
                            Rendered::Failed => {
                                shared.have.insert((index, kind), HaveSlot::Failed(spec));
                                trace::emit(
                                    rendition_context,
                                    "photo.ui.reject",
                                    &[Field::Str("reason", "render_failed")],
                                );
                            }
                            Rendered::Cancelled => {
                                trace::emit(
                                    rendition_context,
                                    "photo.ui.reject",
                                    &[Field::Str("reason", "render_cancelled")],
                                );
                            }
                        }
                    } else {
                        trace::emit(
                            rendition_context,
                            "photo.ui.reject",
                            &[Field::Str("reason", "stale_demand")],
                        );
                    }
                }
                if step.card_done == shared.want.card && step.card_done.is_some() {
                    shared.card_batch = CardBatch::Settled;
                }
                drop(shared);
                let f = app.global::<Frame>();
                let model = f.get_photos();
                for (index, kind, buf) in admitted {
                    let rendition_context = if buf.is_some() {
                        context.map(|c| c.rendition(kind.label()))
                    } else {
                        None
                    };
                    let Some(mut photo) = model.row_data(index) else {
                        trace::emit(
                            rendition_context,
                            "photo.ui.reject",
                            &[Field::Str("reason", "row_missing")],
                        );
                        continue;
                    };
                    let published = buf.is_some();
                    let image = buf.map(slint::Image::from_rgb8).unwrap_or_default();
                    match kind {
                        Kind::Card => {
                            if let (
                                true,
                                Some(Card {
                                    size,
                                    bake,
                                    generation,
                                }),
                            ) = (published, card)
                            {
                                f.set_photo_scrim_baked(bake);
                                LOAD_STATE.lock().unwrap().ready(generation, size);
                            }
                            photo.image = image;
                        }
                        Kind::Full => photo.full = image,
                        Kind::Thumb => photo.thumb = image,
                    }
                    model.set_row_data(index, photo);
                    if published {
                        trace::emit(rendition_context, "photo.ui.apply", &[]);
                    }
                }
                prewarm::refresh(&app);
                trace::duration(context, "photo.ui.end", apply_started, &[]);
                if let Some(Card { generation, .. }) = step.card_done {
                    LOAD_STATE.lock().unwrap().finish(generation);
                }
            }
        })
        .is_err()
        {
            trace::emit(
                context,
                "photo.ui.reject",
                &[Field::Str("reason", "enqueue_failed")],
            );
            return;
        }
        let started = trace::start(context);
        let _ = acknowledged.recv();
        trace::duration(context, "photo.ui.ack_wait", started, &[]);
        if let (Some(store), Some(pending)) = (store(), pending) {
            store.writer.enqueue(pending);
        }
        trace::duration(context, "photo.job.end", job_started, &[]);
        if let Some(index) = claimed_index {
            SHARED.lock().unwrap().release(index);
            DEMAND_CHANGED.notify_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        composite_scrim, cover, dct_scale, decode_oriented, exif_date, fit, oriented_dimensions,
        raster_scrim, render, render_with_store, save_pending_full, scan, taken, threads_from,
        warm_stored, window, Card, CardBatch, HaveSlot, Job, Kind, Rendered, Shared, ThumbStore,
        ThumbnailCache, ThumbnailKey, Want, Warm, MAX_THUMBS, THUMB,
    };
    use image::metadata::Orientation;
    use image::{imageops, Rgb, RgbImage};
    use slint::{Rgb8Pixel, SharedPixelBuffer};
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, SystemTime};

    struct PhotoFixture(PathBuf);

    impl PhotoFixture {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            // Parallel tests can read the same clock value; the counter keeps names distinct.
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "photoframe-photos-{}-{unique}-{serial}",
                std::process::id()
            ));
            std::fs::create_dir(&dir).unwrap();
            let image = RgbImage::from_fn(48, 32, |x, y| Rgb([(x * 5) as u8, (y * 7) as u8, 83]));
            let file = std::fs::File::create(dir.join("photo.jpg")).unwrap();
            image::codecs::jpeg::JpegEncoder::new_with_quality(file, 100)
                .encode_image(&image)
                .unwrap();
            Self(dir)
        }

        fn photo(&self) -> PathBuf {
            self.0.join("photo.jpg")
        }
    }

    impl Drop for PhotoFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn full_decode(path: &std::path::Path) -> RgbImage {
        decode_oriented(path, (u32::MAX / 2, u32::MAX / 2), None)
            .unwrap()
            .image
    }

    fn with_orientation(jpeg: &[u8], orientation: u8) -> Vec<u8> {
        let mut exif = b"Exif\0\0".to_vec();
        exif.extend_from_slice(&[
            b'I',
            b'I',
            42,
            0,
            8,
            0,
            0,
            0,
            1,
            0,
            0x12,
            1,
            3,
            0,
            1,
            0,
            0,
            0,
            orientation,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ]);
        let mut tagged = jpeg[..2].to_vec();
        tagged.extend_from_slice(&[0xff, 0xe1]);
        tagged.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        tagged.extend_from_slice(&exif);
        tagged.extend_from_slice(&jpeg[2..]);
        tagged
    }

    fn large_jpeg(dir: &std::path::Path, name: &str, w: u32, h: u32) -> PathBuf {
        let image = RgbImage::from_fn(w, h, |x, y| {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            Rgb([
                (127.0 + 120.0 * (fx * 9.0).sin()) as u8,
                (127.0 + 120.0 * (fy * 7.0 + fx * 3.0).cos()) as u8,
                (fx * 255.0) as u8,
            ])
        });
        let path = dir.join(name);
        image::codecs::jpeg::JpegEncoder::new_with_quality(
            std::fs::File::create(&path).unwrap(),
            92,
        )
        .encode_image(&image)
        .unwrap();
        path
    }

    fn mean_abs_error(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        a.iter()
            .zip(b)
            .map(|(&a, &b)| f64::from(a.abs_diff(b)))
            .sum::<f64>()
            / a.len() as f64
    }

    #[test]
    fn dct_scale_picks_the_smallest_scale_that_still_covers_the_target() {
        assert_eq!(dct_scale((4000, 3000), (270, 270)), 1);
        assert_eq!(dct_scale((4000, 3000), (388, 280)), 1);
        assert_eq!(dct_scale((4000, 3000), (800, 1280)), 4);
        assert_eq!(dct_scale((4000, 3000), (1280, 800)), 4);
        assert_eq!(dct_scale((4000, 3000), (2100, 1500)), 8);
        assert_eq!(dct_scale((4000, 3000), (4000, 3000)), 8);
        assert_eq!(dct_scale((640, 480), (800, 1280)), 8);
        assert_eq!(dct_scale((8000, 6000), (100, 100)), 1);
        assert_eq!(dct_scale((10000, 100), (800, 1280)), 8);
    }

    #[test]
    fn dct_scale_output_is_never_smaller_than_the_cover_size() {
        for (w, h) in [
            (4000, 3000),
            (3000, 4000),
            (5472, 3648),
            (1999, 1001),
            (4001, 17),
        ] {
            for target in [(270, 270), (388, 280), (800, 1280), (1280, 800), (33, 700)] {
                let idct = dct_scale((w, h), target);
                let scale = (target.0 as f32 / w as f32).max(target.1 as f32 / h as f32);
                let covered = |len: u32, out: u32| out >= (len as f32 * scale).ceil() as u32;
                let out = |len: u32| (len * idct).div_ceil(8);
                assert!(
                    scale >= 1.0 || (covered(w, out(w)) && covered(h, out(h))),
                    "{w}x{h} -> {target:?} at {idct}/8"
                );
            }
        }
    }

    #[test]
    fn quarter_turn_orientations_swap_the_dimensions_used_for_the_scale() {
        let source = (4000, 3000);
        for orientation in [Orientation::Rotate90, Orientation::Rotate270FlipH] {
            let oriented = oriented_dimensions(source, orientation);
            assert_eq!(oriented, (3000, 4000));
            assert_eq!(dct_scale(oriented, (800, 1280)), 4);
            assert_eq!(dct_scale(source, (800, 1280)), 4);
            assert_eq!(dct_scale(oriented, (200, 1000)), 2);
            assert_eq!(dct_scale(source, (200, 1000)), 4);
        }
        assert_eq!(oriented_dimensions(source, Orientation::Rotate180), source);
        assert_eq!(
            oriented_dimensions(source, Orientation::FlipHorizontal),
            source
        );
    }

    #[test]
    fn scaled_decode_matches_the_full_decode_path() {
        let fixture = PhotoFixture::new();
        let path = large_jpeg(&fixture.0, "large.jpg", 1600, 1000);
        let jpeg = std::fs::read(&path).unwrap();
        let rotated = fixture.0.join("rotated.jpg");
        std::fs::write(&rotated, with_orientation(&jpeg, 6)).unwrap();
        for (path, sizes) in [
            (&path, [(270, 270), (388, 280), (640, 400)]),
            (&rotated, [(270, 270), (388, 280), (400, 640)]),
        ] {
            let full = full_decode(path);
            for (w, h) in sizes {
                let scaled = decode_oriented(path, (w, h), None).unwrap();
                assert!(scaled.idct < 8, "{w}x{h} should decode scaled");
                assert!(scaled.image.width() < full.width());
                let expected = fit(&full, w, h, || true).unwrap();
                let actual = fit(&scaled.image, w, h, || true).unwrap();
                assert_eq!(actual.width(), expected.width());
                assert_eq!(actual.height(), expected.height());
                let error = mean_abs_error(actual.as_bytes(), expected.as_bytes());
                assert!(error < 2.0, "{w}x{h}: mean abs error {error}");
            }
        }
    }

    #[test]
    fn grayscale_jpeg_decodes_scaled_as_rgb() {
        let fixture = PhotoFixture::new();
        let path = fixture.0.join("gray.jpg");
        let gray =
            image::GrayImage::from_fn(1600, 1200, |x, y| image::Luma([((x + y) / 13) as u8]));
        image::codecs::jpeg::JpegEncoder::new_with_quality(
            std::fs::File::create(&path).unwrap(),
            92,
        )
        .encode_image(&gray)
        .unwrap();
        let scaled = decode_oriented(&path, (200, 200), None).unwrap();
        assert!(scaled.idct < 8);
        let full = full_decode(&path);
        assert_eq!(full.dimensions(), (1600, 1200));
        let (sw, sh) = scaled.image.dimensions();
        let mid = scaled.image.get_pixel(sw / 2, sh / 2);
        let reference = full.get_pixel(1600 / 2, 1200 / 2);
        assert_eq!(mid[0], mid[1]);
        assert!(mid[0].abs_diff(reference[0]) < 6);
    }

    #[test]
    fn render_produces_every_rendition_from_one_large_photo() {
        let fixture = PhotoFixture::new();
        let path = large_jpeg(&fixture.0, "large.jpg", 1600, 1000);
        let full = full_decode(&path);
        let specs = vec![
            (Kind::Thumb, (THUMB, THUMB, 0)),
            (Kind::Card, (388, 280, 0)),
            (Kind::Full, (800, 1280, 0)),
        ];
        let job = Job {
            trace: None,
            index: 0,
            specs,
        };
        let (out, _) = render(&path, &job, |_, _| true);
        assert_eq!(out.len(), 3);
        for (_, (w, h, _), rendered) in out {
            let Rendered::Ready(buffer) = rendered else {
                panic!("rendition failed");
            };
            assert_eq!((buffer.width(), buffer.height()), (w, h));
            let expected = fit(&full, w, h, || true).unwrap();
            let error = mean_abs_error(buffer.as_bytes(), expected.as_bytes());
            assert!(error < 2.0, "{w}x{h}: mean abs error {error}");
        }
    }

    #[test]
    fn full_store_hit_skips_original_decode_and_corrupt_entry_falls_back() {
        let fixture = PhotoFixture::new();
        let path = fixture.photo();
        let store = ThumbStore::new(fixture.0.join("cache"));
        let spec = (24, 32, 0);
        let job = Job {
            trace: None,
            index: 0,
            specs: vec![(Kind::Full, spec)],
        };
        let (original, _) = render_with_store(&path, &job, |_, _| true, None);
        let Rendered::Ready(expected) = &original[0].2 else {
            panic!("original failed")
        };
        let key = ThumbnailKey::read(&path, spec, false).unwrap();
        // Remove this unique fixture from RAM so the next render must read disk.
        super::RENDITIONS
            .lock()
            .unwrap()
            .entries
            .retain(|(k, _)| k != &key);
        store
            .save_full(&key.stored_full_name(), expected, None)
            .unwrap();
        let loaded = store
            .load_full(&key.stored_full_name(), spec.0, spec.1)
            .unwrap();
        let error = mean_abs_error(expected.as_bytes(), loaded.as_bytes());
        assert!(error < 3.0, "JPEG mean absolute error {error}");
        let (cached, _) = render_with_store(&path, &job, |_, _| true, Some(&store));
        let Rendered::Ready(actual) = &cached[0].2 else {
            panic!("cached render failed")
        };
        assert_eq!(actual.as_bytes(), loaded.as_bytes());

        // A corrupt store entry is rebuilt from the still-valid source.
        {
            let mut cache = super::RENDITIONS.lock().unwrap();
            cache.entries.retain(|(k, _)| k != &key);
            cache.bytes = cache
                .entries
                .iter()
                .map(|(_, image)| image.as_bytes().len())
                .sum();
        }
        std::fs::write(
            fixture.0.join("cache/full").join(key.stored_full_name()),
            b"bad jpeg",
        )
        .unwrap();
        let (rebuilt, pending) = render_with_store(&path, &job, |_, _| true, Some(&store));
        let Rendered::Ready(rebuilt) = &rebuilt[0].2 else {
            panic!("source fallback failed")
        };
        assert_eq!(rebuilt.as_bytes(), expected.as_bytes());
        assert!(store
            .load_full(&key.stored_full_name(), spec.0, spec.1)
            .is_none());
        save_pending_full(&store, pending.unwrap());
        assert!(store
            .load_full(&key.stored_full_name(), spec.0, spec.1)
            .is_some());

        // A valid key for an undecodable source still succeeds from disk.
        std::fs::write(&path, b"not an image").unwrap();
        let broken_key = ThumbnailKey::read(&path, spec, false).unwrap();
        store
            .save_full(&broken_key.stored_full_name(), expected, None)
            .unwrap();
        let (cached, _) = render_with_store(&path, &job, |_, _| true, Some(&store));
        assert!(matches!(cached[0].2, Rendered::Ready(_)));
        super::RENDITIONS
            .lock()
            .unwrap()
            .entries
            .retain(|(k, _)| k != &broken_key);
        std::fs::write(
            fixture
                .0
                .join("cache/full")
                .join(broken_key.stored_full_name()),
            b"bad jpeg",
        )
        .unwrap();
        let (failed, _) = render_with_store(&path, &job, |_, _| true, Some(&store));
        assert!(matches!(failed[0].2, Rendered::Failed));
    }

    #[test]
    fn trace_correlates_renditions_and_deferred_write_without_source_content() {
        const CHILD: &str = "FRAME_PHOTO_TRACE_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let _guard = frame_trace::init_from_env();
            assert!(frame_trace::enabled());
            let fixture = PhotoFixture::new();
            let store = ThumbStore::new(fixture.0.join("cache"));
            let job = Job {
                trace: super::trace::Context::new(7, false),
                index: 7,
                specs: vec![(Kind::Full, (32, 24, 0)), (Kind::Thumb, (12, 12, 0))],
            };
            super::trace::duration(
                job.trace,
                "photo.fields_probe",
                Some(std::time::Instant::now()),
                &[
                    frame_trace::Field::Bool("a", true),
                    frame_trace::Field::Bool("b", true),
                    frame_trace::Field::Bool("c", true),
                    frame_trace::Field::Bool("d", true),
                    frame_trace::Field::Bool("e", true),
                    frame_trace::Field::Bool("f", true),
                    frame_trace::Field::Bool("g", true),
                    frame_trace::Field::Bool("h", true),
                    frame_trace::Field::Bool("i", true),
                ],
            );
            let (outcomes, pending) =
                render_with_store(&fixture.photo(), &job, |_, _| true, Some(&store));
            assert!(outcomes
                .iter()
                .all(|(_, _, rendered)| matches!(rendered, Rendered::Ready(_))));
            let pending = pending.unwrap();
            assert_eq!(pending.trace.unwrap().id, job.trace.unwrap().id);
            let (done, finished) = std::sync::mpsc::channel();
            let writer = super::cache_writer::CacheWriter::new(1024 * 1024, 2, move |pending| {
                save_pending_full(&store, pending);
                done.send(()).unwrap();
            });
            writer.enqueue(pending);
            finished.recv().unwrap();
            drop(writer);
            return;
        }
        // The exporter is process-global. A child keeps this test isolated from other tests.
        let fixture = PhotoFixture::new();
        let output = fixture.0.join("trace.jsonl");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "photos::tests::trace_correlates_renditions_and_deferred_write_without_source_content", "--test-threads=1"])
            .env(CHILD, "1")
            .env("FRAME_TRACE_OUTPUT", &output)
            .env("FRAME_TRACE_SECONDS", "5")
            .status()
            .unwrap();
        assert!(status.success());
        let wire = std::fs::read_to_string(output).unwrap();
        let summary: serde_json::Value =
            serde_json::from_str(wire.lines().last().unwrap()).unwrap();
        assert_eq!(summary["truncated_fields"], 1);
        assert!(!wire.contains("photo.jpg"));
        assert!(!wire.contains("photoframe-photos-"));
        let events: Vec<serde_json::Value> = wire
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .filter(|value: &serde_json::Value| value["type"] == "event")
            .collect();
        assert!(!events.is_empty());
        let probe = events
            .iter()
            .find(|event| event["name"] == "photo.fields_probe")
            .unwrap();
        assert_eq!(probe["fields"].as_object().unwrap().len(), 12);
        assert_eq!(probe["fields"]["h"], true);
        assert!(probe["fields"]["i"].is_null());
        let id = events[0]["op_id"].as_u64().unwrap();
        assert!(events.iter().all(|event| event["op_id"] == id
            && event["fields"]["source_index"] == 7
            && event["fields"]["warm"] == false));
        assert_eq!(
            events
                .iter()
                .filter(|event| event["name"] == "photo.rendition.end"
                    && event["fields"]["result"] == "ready")
                .count(),
            2
        );
        for name in [
            "photo.cache.ram",
            "photo.cache.disk",
            "photo.original.read",
            "photo.original.decode",
            "photo.fit",
            "photo.store.defer",
            "photo.store.queue",
            "photo.store.queue_wait",
            "photo.store.encode",
            "photo.store.flush",
            "photo.store.fsync",
            "photo.store.rename",
            "photo.store.prune",
        ] {
            assert!(
                events.iter().any(|event| event["name"] == name),
                "missing {name}"
            );
        }
        let deferred = events
            .iter()
            .position(|event| event["name"] == "photo.store.defer")
            .unwrap();
        let written = events
            .iter()
            .position(|event| {
                event["name"] == "photo.store.write" && event["fields"]["kind"] == "full"
            })
            .unwrap();
        assert!(deferred < written);
        let admitted = events
            .iter()
            .position(|event| {
                event["name"] == "photo.store.queue" && event["fields"]["result"] == "admitted"
            })
            .unwrap();
        let started = events
            .iter()
            .position(|event| event["name"] == "photo.store.queue_wait")
            .unwrap();
        assert!(admitted < started && started < written);
    }

    #[test]
    fn pending_full_write_is_discarded_when_source_changes() {
        let fixture = PhotoFixture::new();
        let path = fixture.photo();
        let store = ThumbStore::new(fixture.0.join("cache"));
        let spec = (24, 32, 0);
        let job = Job {
            trace: None,
            index: 0,
            specs: vec![(Kind::Full, spec)],
        };
        let (outcomes, pending) = render_with_store(&path, &job, |_, _| true, Some(&store));
        assert!(matches!(outcomes[0].2, Rendered::Ready(_)));
        let pending = pending.unwrap();
        let name = pending.key.stored_full_name();
        assert!(store.load_full(&name, spec.0, spec.1).is_none());
        let (started, waiting) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let (done, finished) = std::sync::mpsc::channel();
        let cache_dir = fixture.0.join("cache");
        let writer = super::cache_writer::CacheWriter::new(1024 * 1024, 2, move |pending| {
            started.send(()).unwrap();
            released.recv().unwrap();
            save_pending_full(&ThumbStore::new(cache_dir.clone()), pending);
            done.send(()).unwrap();
        });
        writer.enqueue(pending);
        waiting.recv().unwrap();
        std::fs::write(&path, b"changed source").unwrap();
        release.send(()).unwrap();
        finished.recv().unwrap();
        assert!(store.load_full(&name, spec.0, spec.1).is_none());
    }

    #[test]
    fn jpeg_exif_rotation_and_mirroring_are_applied_before_cover() {
        let fixture = PhotoFixture::new();
        let original = full_decode(&fixture.photo());
        let jpeg = std::fs::read(fixture.photo()).unwrap();
        for orientation in [2u8, 6] {
            let tagged = with_orientation(&jpeg, orientation);
            let path = fixture.0.join(format!("orientation-{orientation}.jpg"));
            std::fs::write(&path, tagged).unwrap();
            let image = full_decode(&path);
            assert_eq!(
                image.dimensions(),
                if orientation == 6 { (32, 48) } else { (48, 32) }
            );
            for (x, y, pixel) in original.enumerate_pixels() {
                let target = if orientation == 6 {
                    (31 - y, x)
                } else {
                    (47 - x, y)
                };
                assert_eq!(image.get_pixel(target.0, target.1), pixel);
            }
            let thumbnail = cover(&path, image.width(), image.height(), || true).unwrap();
            assert_eq!(thumbnail.as_bytes(), image.as_raw());
        }
    }

    #[test]
    fn cover_keeps_original_center_crop_pixels_at_exact_target_sizes() {
        let fixture = PhotoFixture::new();
        let image = full_decode(&fixture.photo());
        for (w, h) in [(48, 32), (24, 24), (14, 42), (47, 30)] {
            let scale = (w as f32 / image.width() as f32).max(h as f32 / image.height() as f32);
            let scaled = imageops::resize(
                &image,
                (image.width() as f32 * scale).ceil() as u32,
                (image.height() as f32 * scale).ceil() as u32,
                imageops::FilterType::Triangle,
            );
            let expected = imageops::crop_imm(
                &scaled,
                (scaled.width() - w) / 2,
                (scaled.height() - h) / 2,
                w,
                h,
            )
            .to_image();
            let actual = cover(&fixture.photo(), w, h, || true).unwrap();
            assert_eq!((actual.width(), actual.height()), (w, h));
            assert_eq!(actual.as_bytes(), expected.as_raw());
        }
        assert!(cover(&fixture.photo(), 24, 24, || false).is_none());
    }

    #[test]
    fn thumbnail_cache_reuses_size_roundtrip_and_invalidates_file_or_scrim_changes() {
        let fixture = PhotoFixture::new();
        let path = fixture.photo();
        let first = ThumbnailKey::read(&path, (24, 24, 10), false).unwrap();
        assert_eq!(
            ThumbnailKey::read(&fixture.0.join(".").join("photo.jpg"), first.size, false).unwrap(),
            first
        );
        let mut cache = ThumbnailCache::new(16, 16 * 1024 * 1024);
        let image = cover(&path, 24, 24, || true).unwrap();
        cache.insert(first.clone(), image.clone());
        let second = ThumbnailKey::read(&path, (14, 42, 10), false).unwrap();
        cache.insert(second.clone(), cover(&path, 14, 42, || true).unwrap());
        assert_eq!(
            cache.get(&first).unwrap().as_bytes().as_ptr(),
            image.as_bytes().as_ptr()
        );
        assert!(cache
            .get(&ThumbnailKey::read(&path, first.size, true).unwrap())
            .is_none());
        assert!(cache
            .get(&ThumbnailKey::read(&path, (24, 24, 11), false).unwrap())
            .is_none());
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(first.modified + Duration::from_secs(2)),
            )
            .unwrap();
        let updated = ThumbnailKey::read(&path, first.size, false).unwrap();
        assert_ne!(first, updated);
        assert!(cache.get(&updated).is_none());
        cache.insert(updated.clone(), image.clone());
        assert!(cache.get(&first).is_none());
        assert!(cache.get(&second).is_none());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&[0])
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(updated.modified))
            .unwrap();
        assert!(cache
            .get(&ThumbnailKey::read(&path, first.size, false).unwrap())
            .is_none());
    }

    #[test]
    fn thumbnail_cache_evicts_least_recent_entries_and_bounds_pixel_memory() {
        let fixture = PhotoFixture::new();
        let mut cache = ThumbnailCache::new(16, 16 * 1024 * 1024);
        let first = ThumbnailKey::read(&fixture.photo(), (1, 1, 0), false).unwrap();
        for w in 1..=16 {
            let key = ThumbnailKey::read(&fixture.photo(), (w, 1, 0), false).unwrap();
            cache.insert(key, SharedPixelBuffer::new(w, 1));
        }
        assert!(cache.get(&first).is_some());
        cache.insert(
            ThumbnailKey::read(&fixture.photo(), (17, 1, 0), false).unwrap(),
            SharedPixelBuffer::new(17, 1),
        );
        assert!(cache.get(&first).is_some());
        assert!(cache
            .get(&ThumbnailKey::read(&fixture.photo(), (2, 1, 0), false).unwrap())
            .is_none());
        for height in 1024..1030 {
            cache.insert(
                ThumbnailKey::read(&fixture.photo(), (1024, height, 0), false).unwrap(),
                SharedPixelBuffer::new(1024, height),
            );
            assert!(cache.bytes <= cache.max_bytes);
            assert!(cache.entries.len() <= cache.max_entries);
        }
        assert!(cache.get(&first).is_none());
    }

    #[test]
    fn canonical_svg_preserves_top_pixels_and_darkens_only_bottom_strip() {
        let scrim = raster_scrim(388, 180).unwrap();
        let white = Rgb8Pixel {
            r: 255,
            g: 255,
            b: 255,
        };
        let mut photo = SharedPixelBuffer::<Rgb8Pixel>::new(388, 280);
        photo.make_mut_slice().fill(white);
        composite_scrim(&mut photo, &scrim);
        let pixels = photo.as_slice();
        assert_eq!(pixels[0], white);
        assert_eq!(pixels[120 * 388 + 100], white);
        let bottom = pixels[279 * 388 + 100];
        assert!((96..=100).contains(&bottom.r));
        assert!((95..=100).contains(&bottom.g));
        assert!((94..=100).contains(&bottom.b));
        assert!((177..=179).contains(&scrim.get_pixel(100, 179)[3]));
    }

    #[test]
    fn source_over_uses_straight_alpha_without_darkening_transparent_pixels() {
        let mut photo = SharedPixelBuffer::<Rgb8Pixel>::new(2, 1);
        photo.make_mut_slice().fill(Rgb8Pixel {
            r: 200,
            g: 100,
            b: 50,
        });
        let scrim = image::RgbaImage::from_raw(2, 1, vec![20, 40, 60, 0, 20, 40, 60, 128]).unwrap();
        composite_scrim(&mut photo, &scrim);
        assert_eq!(
            photo.as_slice()[0],
            Rgb8Pixel {
                r: 200,
                g: 100,
                b: 50
            }
        );
        assert_eq!(
            photo.as_slice()[1],
            Rgb8Pixel {
                r: 110,
                g: 70,
                b: 55
            }
        );
    }

    #[test]
    fn caption_is_month_and_year_from_exif_date_taken() {
        let date = b"2023:03:14 10:20:30\0";
        let mut tiff = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        tiff.extend_from_slice(&[1, 0, 0x69, 0x87, 4, 0, 1, 0, 0, 0, 26, 0, 0, 0, 0, 0, 0, 0]);
        tiff.extend_from_slice(&[1, 0, 0x03, 0x90, 2, 0, 20, 0, 0, 0, 44, 0, 0, 0, 0, 0, 0, 0]);
        tiff.extend_from_slice(date);
        assert_eq!(exif_date(&tiff).as_deref(), Some("2023:03:14 10:20:30"));
        let mut tagged = b"Exif\0\0".to_vec();
        tagged.extend_from_slice(&tiff);
        assert_eq!(exif_date(&tagged).as_deref(), Some("2023:03:14 10:20:30"));
        assert_eq!(exif_date(b"nonsense"), None);
        assert_eq!(exif_date(&[b'I', b'I', 42, 0, 255, 255, 255, 255]), None);

        let fixture = PhotoFixture::new();
        assert_eq!(taken(&fixture.photo()), None);
        let jpeg = std::fs::read(fixture.photo()).unwrap();
        let mut file = jpeg[..2].to_vec();
        file.extend_from_slice(&[0xff, 0xe1]);
        file.extend_from_slice(&((tagged.len() + 2) as u16).to_be_bytes());
        file.extend_from_slice(&tagged);
        file.extend_from_slice(&jpeg[2..]);
        let dated = fixture.0.join("a-dated.jpg");
        std::fs::write(&dated, file).unwrap();
        assert_eq!(
            scan(&fixture.0)
                .iter()
                .map(|e| e.caption.as_str())
                .collect::<Vec<_>>(),
            ["", "March 2023"]
        );

        let mut filled = jpeg[..2].to_vec();
        filled.extend_from_slice(&[0xff, 0xff, 0xff, 0xe1]);
        filled.extend_from_slice(&((tagged.len() + 2) as u16).to_be_bytes());
        filled.extend_from_slice(&tagged);
        filled.extend_from_slice(&jpeg[2..]);
        let padded = fixture.0.join("b-fill-bytes.jpg");
        std::fs::write(&padded, filled).unwrap();
        assert_eq!(taken(&padded), taken(&dated));
        assert!(taken(&padded).is_some());
    }

    #[test]
    fn warm_pass_stores_once_and_yields_to_display_work() {
        let fixture = PhotoFixture::new();
        let store = ThumbStore::new(fixture.0.join("thumbs"));
        let name = ThumbnailKey::read(&fixture.photo(), (THUMB, THUMB, 0), false)
            .unwrap()
            .stored_name();
        assert!(!warm_stored(&store, &fixture.photo(), || true, None));
        assert!(!store.contains(&name));
        assert!(warm_stored(&store, &fixture.photo(), || false, None));
        assert!(store.load(&name, THUMB).is_some());
        assert!(warm_stored(
            &store,
            &fixture.photo(),
            || panic!("stored photo must not decode"),
            None
        ));
    }

    fn resident_want(current: usize) -> Want {
        Want {
            current,
            card: Some(Card {
                size: (388, 280, 180),
                bake: true,
                generation: 1,
            }),
            screen: Some((800, 1280)),
            full: true,
            thumbs: 0..21,
            ..Want::default()
        }
    }

    fn resident_bytes(count: usize) -> usize {
        let mut shared = Shared::new();
        shared.want = resident_want(0);
        let mut peak = 0;
        for current in 0..count {
            shared.want.current = current;
            loop {
                let step = shared.step(count);
                for key in step.evict {
                    shared.have.remove(&key);
                }
                let Some(job) = step.job else { break };
                for (kind, spec) in job.specs {
                    shared.have.insert((job.index, kind), HaveSlot::Ready(spec));
                }
                peak = peak.max(
                    shared
                        .have
                        .values()
                        .map(|slot| {
                            let (w, h, _) = slot.size();
                            w as usize * h as usize * 3
                        })
                        .sum(),
                );
            }
        }
        peak
    }

    #[test]
    fn resident_pixels_stay_bounded_as_the_photo_count_grows() {
        let urgent = window(0, 66);
        assert_eq!(urgent, [0, 1, 65, 2]);
        assert_eq!(window(65, 66), [65, 0, 64, 1]);
        assert_eq!(window(0, 2), [0, 1]);
        assert!(window(0, 0).is_empty());
        let small = resident_bytes(30);
        let large = resident_bytes(2000);
        let want = resident_want(0);
        let (card_w, card_h, _) = want.card.unwrap().size;
        let (screen_w, screen_h) = want.screen.unwrap();
        let window_bytes =
            (card_w as usize * card_h as usize + screen_w as usize * screen_h as usize) * 3;
        let thumb_bytes = want.thumbs.clone().count() * THUMB as usize * THUMB as usize * 3;
        let limit = urgent.len() * window_bytes + thumb_bytes;
        assert_eq!(small, large);
        assert!(large <= limit, "{large} > {limit}");
    }

    #[test]
    fn rapid_grid_jump_keeps_both_images_actually_in_transition_and_grid_resident() {
        let want = Want {
            current: 40,
            card_displayed: vec![1, 0],
            full_displayed: vec![9, 8],
            card: Some(Card {
                size: (388, 280, 180),
                bake: true,
                generation: 1,
            }),
            screen: Some((800, 1280)),
            full: true,
            thumbs: 0..2000,
        };
        let slots = want.slots(2000);
        for index in [0, 1] {
            assert!(slots.contains(&(index, Kind::Card, (388, 280, 180))));
        }
        for index in [8, 9] {
            assert!(slots.contains(&(index, Kind::Full, (800, 1280, 0))));
        }
        assert_eq!(
            slots.iter().filter(|s| s.1 == Kind::Thumb).count(),
            MAX_THUMBS
        );
        assert!(
            slots.iter().position(|s| s.1 == Kind::Thumb)
                > slots.iter().rposition(|s| s.1 != Kind::Thumb)
        );
        assert_eq!(slots.iter().filter(|s| s.1 == Kind::Card).count(), 2);
        assert_eq!(slots.iter().filter(|s| s.1 == Kind::Full).count(), 6);
    }

    #[test]
    fn slideshow_loads_requested_frame_before_hidden_cards_and_grid() {
        let mut shared = Shared::new();
        shared.want = Want {
            current: 0,
            full_displayed: vec![65],
            card: Some(Card {
                size: (388, 280, 180),
                bake: true,
                generation: 1,
            }),
            screen: Some((800, 1280)),
            full: true,
            thumbs: 0..21,
            ..Want::default()
        };
        let step = shared.step(66);
        let job = step.job.unwrap();
        assert_eq!(job.index, 0);
        assert_eq!(
            job.specs,
            [
                (Kind::Full, (800, 1280, 0)),
                (Kind::Thumb, (THUMB, THUMB, 0))
            ]
        );
        let slots = shared.want.slots(66);
        assert!(slots.contains(&(65, Kind::Full, (800, 1280, 0))));
        assert!(!slots.iter().any(|slot| slot.1 == Kind::Card));
        shared.want.full = false;
        let slots = shared.want.slots(66);
        assert_eq!(slots[0], (0, Kind::Card, (388, 280, 180)));
        assert_eq!(
            slots.iter().filter(|slot| slot.1 == Kind::Thumb).count(),
            21
        );
        shared.want.thumbs = 0..2000;
        assert_eq!(
            shared
                .want
                .slots(2000)
                .iter()
                .filter(|slot| slot.1 == Kind::Thumb)
                .count(),
            MAX_THUMBS
        );
    }

    fn fill(shared: &mut Shared, job: &Job) {
        for &(kind, spec) in &job.specs {
            shared.have.insert((job.index, kind), HaveSlot::Ready(spec));
        }
    }

    fn card_want() -> Want {
        Want {
            card: Some(Card {
                size: (388, 280, 180),
                bake: true,
                generation: 1,
            }),
            ..Want::default()
        }
    }

    #[test]
    fn parallel_claims_take_different_photos_and_never_repeat_one_in_flight() {
        let mut shared = Shared::new();
        shared.want = resident_want(0);
        let first = shared.claim(66).job.unwrap();
        let second = shared.claim(66).job.unwrap();
        assert_ne!(first.index, second.index);
        let mut claimed = vec![first.index, second.index];
        while let Some(job) = shared.claim(66).job {
            claimed.push(job.index);
        }
        let total = claimed.len();
        claimed.sort_unstable();
        claimed.dedup();
        assert_eq!(claimed.len(), total);
        assert_eq!([first.index, second.index], [0, 1]);
        shared.release(first.index);
        assert_eq!(shared.claim(66).job.unwrap().index, first.index);
    }

    #[test]
    fn in_flight_job_can_be_cancelled_when_demand_drops_its_slot() {
        let mut shared = Shared::new();
        shared.want = resident_want(0);
        let job = shared.claim(66).job.unwrap();
        assert!(shared.accepts(job.index, Kind::Full, (800, 1280, 0), 66));
        shared.want = resident_want(30);
        assert!(!shared.accepts(job.index, Kind::Full, (800, 1280, 0), 66));
        assert_eq!(shared.in_flight, [job.index]);
        shared.release(job.index);
        assert!(shared.in_flight.is_empty());
    }

    #[test]
    fn card_batch_settles_once_after_every_parallel_card_job_is_published() {
        let mut shared = Shared::new();
        shared.want = card_want();
        shared.card_batch = CardBatch::Open;
        let first = shared.claim(2).job.unwrap();
        let second = shared.claim(2).job.unwrap();
        let idle = shared.claim(2);
        assert!(idle.job.is_none() && idle.card_done.is_none());
        fill(&mut shared, &first);
        shared.release(first.index);
        assert!(shared.claim(2).card_done.is_none());
        fill(&mut shared, &second);
        shared.release(second.index);
        assert_eq!(shared.claim(2).card_done, shared.want.card);
        assert!(shared.card_batch == CardBatch::Settled);
        assert!(shared.claim(2).card_done.is_none());
    }

    #[test]
    fn single_worker_claims_follow_the_unclaimed_step_order() {
        let mut plain = Shared::new();
        let mut claiming = Shared::new();
        plain.want = resident_want(3);
        claiming.want = resident_want(3);
        loop {
            let expected = plain.step(66).job;
            let actual = claiming.claim(66).job;
            assert_eq!(
                expected.as_ref().map(|j| (j.index, j.specs.clone())),
                actual.as_ref().map(|j| (j.index, j.specs.clone()))
            );
            let (Some(expected), Some(actual)) = (expected, actual) else {
                break;
            };
            fill(&mut plain, &expected);
            fill(&mut claiming, &actual);
            claiming.release(actual.index);
        }
    }

    #[test]
    fn warm_pass_hands_each_photo_out_once_and_returns_preempted_ones() {
        let mut warm = Warm::new();
        assert_eq!(
            [warm.claim(3), warm.claim(3), warm.claim(3), warm.claim(3)],
            [Some(0), Some(1), Some(2), None]
        );
        warm.retry.push(1);
        assert_eq!(warm.claim(3), Some(1));
        assert_eq!(warm.claim(3), None);
    }

    #[test]
    fn decode_thread_count_defaults_to_two_and_clamps() {
        assert_eq!(threads_from(None), 2);
        assert_eq!(threads_from(Some("x")), 2);
        assert_eq!(threads_from(Some("0")), 1);
        assert_eq!(threads_from(Some("1")), 1);
        assert_eq!(threads_from(Some("9")), 3);
    }

    #[test]
    fn queued_decode_survives_unrelated_demand_but_requires_exact_slot_size() {
        let mut shared = Shared::new();
        shared.want.current = 4;
        shared.want.card = Some(Card {
            size: (388, 280, 180),
            bake: true,
            generation: 1,
        });
        shared.want.thumbs = 0..12;
        assert!(shared.accepts(4, Kind::Card, (388, 280, 180), 20));
        assert!(shared.accepts(10, Kind::Thumb, (THUMB, THUMB, 0), 20));
        shared.want.thumbs = 4..16;
        assert!(shared.accepts(10, Kind::Thumb, (THUMB, THUMB, 0), 20));
        assert!(shared.accepts(4, Kind::Card, (388, 280, 180), 20));
        assert!(!shared.accepts(2, Kind::Thumb, (THUMB, THUMB, 0), 20));
        shared.want.card.as_mut().unwrap().bake = false;
        assert!(!shared.accepts(4, Kind::Card, (388, 280, 180), 20));
        assert!(shared.accepts(4, Kind::Card, (388, 280, 0), 20));
        shared.want.current = 15;
        assert!(!shared.accepts(4, Kind::Card, (388, 280, 0), 20));
    }

    #[test]
    fn fit_crops_the_center_of_the_unscaled_image() {
        let image = RgbImage::from_fn(4, 2, |x, _| image::Rgb([x as u8, 0, 0]));
        let output = fit(&image, 2, 2, || true).unwrap();
        let reds: Vec<u8> = output
            .as_bytes()
            .chunks_exact(3)
            .map(|pixel| pixel[0])
            .collect();
        assert_eq!(reds, [1, 2, 1, 2]);
    }

    #[test]
    fn fit_resizes_to_cover_before_cropping_the_center() {
        let image = RgbImage::from_fn(8, 4, |x, _| image::Rgb([if x < 4 { 0 } else { 255 }, 0, 0]));
        let output = fit(&image, 2, 2, || true).unwrap();
        let reds: Vec<u8> = output
            .as_bytes()
            .chunks_exact(3)
            .map(|pixel| pixel[0])
            .collect();
        assert_eq!(reds[0], reds[2]);
        assert!(reds[0] > 0 && reds[1] < 255);
        assert!(reds[0] < reds[1]);
    }

    #[test]
    fn pathological_panorama_and_oversized_target_fail_before_resize_allocation() {
        let image = RgbImage::new(10000, 100);
        assert!(fit(&image, 800, 1280, || true).is_none());
        assert!(fit(&image, 10000, 10000, || true).is_none());
    }
}
