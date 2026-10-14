//! Photo slideshow source. A worker thread decodes and cover-crops only the
//! photos around the one on screen, plus visible grid thumbnails, to the exact
//! pixel size each view draws. Decoded pixels stay bounded; paths and captions
//! grow with the library. Photos come from $FRAME_PHOTOS (default: assets/photos).

use image::imageops::{self, FilterType};
use image::{DynamicImage, ImageDecoder, ImageReader, RgbImage, RgbaImage};
use slint::{ComponentHandle, Model, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Cursor, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::SystemTime;

use crate::{AppWindow, Frame, Photo, Saver};
use crate::photo_load_state::{PhotoLoadState, PhotoSize};

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
    fn read(path: &Path, size: PhotoSize, baked: bool) -> std::io::Result<Self> {
        let path = path.canonicalize()?;
        let metadata = path.metadata()?;
        Ok(Self { path, modified: metadata.modified()?, length: metadata.len(), size, baked })
    }
}

struct ThumbnailCache {
    entries: VecDeque<(ThumbnailKey, SharedPixelBuffer<Rgb8Pixel>)>,
    bytes: usize,
}

impl ThumbnailCache {
    const MAX_BYTES: usize = 16 * 1024 * 1024;
    const MAX_ENTRIES: usize = 16;

    const fn new() -> Self { Self { entries: VecDeque::new(), bytes: 0 } }

    fn get(&mut self, key: &ThumbnailKey) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
        let index = self.entries.iter().position(|(candidate, _)| candidate == key)?;
        let entry = self.entries.remove(index)?;
        let image = entry.1.clone();
        self.entries.push_back(entry);
        Some(image)
    }

    fn insert(&mut self, key: ThumbnailKey, image: SharedPixelBuffer<Rgb8Pixel>) {
        // Remove stale versions of the same source, but retain other render sizes.
        self.entries.retain(|(old, _)| old != &key && (old.path != key.path
            || (old.modified == key.modified && old.length == key.length)));
        self.bytes = self.entries.iter().map(|(_, image)| image.as_bytes().len()).sum();
        let size = image.as_bytes().len();
        if size > Self::MAX_BYTES { return; }
        while self.bytes + size > Self::MAX_BYTES || self.entries.len() >= Self::MAX_ENTRIES {
            if let Some((_, old)) = self.entries.pop_front() { self.bytes -= old.as_bytes().len(); }
        }
        self.bytes += size;
        self.entries.push_back((key, image));
    }
}

static THUMBNAILS: Mutex<ThumbnailCache> = Mutex::new(ThumbnailCache::new());

fn raster_scrim(w: u32, h: u32) -> Result<RgbaImage, &'static str> {
    let image = slint::Image::load_from_svg_data(SCRIM_SVG).map_err(|_| "cannot load caption SVG")?;
    let buffer = image.to_rgba8().ok_or("cannot rasterize caption SVG")?;
    let rgba = RgbaImage::from_raw(buffer.width(), buffer.height(), buffer.as_bytes().to_vec())
        .ok_or("invalid caption SVG raster")?;
    Ok(imageops::resize(&rgba, w, h, FilterType::Triangle))
}

fn composite_scrim(photo: &mut SharedPixelBuffer<Rgb8Pixel>, scrim: &RgbaImage) {
    let start = (photo.height() - scrim.height()) as usize * photo.width() as usize;
    for (background, foreground) in photo.make_mut_slice()[start..].iter_mut().zip(scrim.pixels()) {
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
const BLUR_DIVISOR: u32 = 8;

struct Entry {
    path: PathBuf,
    caption: String,
}

static PLAYLIST: OnceLock<Vec<Entry>> = OnceLock::new();

pub fn count() -> usize { PLAYLIST.get().map_or(0, Vec::len) }

fn bundled_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|exe| exe.parent().map(|parent| parent.join("assets/photos")))
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/photos"))
}

fn dir() -> PathBuf {
    std::env::var_os("FRAME_PHOTOS").filter(|value| !value.is_empty()).map(PathBuf::from)
        .unwrap_or_else(bundled_dir)
}

/// DateTimeOriginal ("YYYY:MM:DD HH:MM:SS") from raw Exif: IFD0 -> Exif IFD -> tag 0x9003.
fn exif_date(exif: &[u8]) -> Option<String> {
    let tiff = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    let little = match tiff.get(..2)? { b"II" => true, b"MM" => false, _ => return None };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };
    // offset of the entry's 4-byte value field
    let value_of = |ifd: usize, tag: u16| -> Option<usize> {
        (0..u16_at(ifd)? as usize).find_map(|i| {
            let entry = ifd.checked_add(2)?.checked_add(i.checked_mul(12)?)?;
            (u16_at(entry) == Some(tag)).then(|| entry.checked_add(8)).flatten()
        })
    };
    let exif_ifd = u32_at(value_of(u32_at(4)? as usize, 0x8769)?)? as usize;
    let text = u32_at(value_of(exif_ifd, 0x9003)?)? as usize;
    std::str::from_utf8(tiff.get(text..text.checked_add(19)?)?).ok().map(str::to_owned)
}

fn taken(path: &Path) -> Option<chrono::NaiveDateTime> {
    let mut decoder = photo_decoder(path).ok()?;
    chrono::NaiveDateTime::parse_from_str(&exif_date(&decoder.exif_metadata().ok()??)?, "%Y:%m:%d %H:%M:%S").ok()
}

/// JPEGs in `dir`, oldest first (undated first, then by name). Caption: "March 2023".
fn scan(dir: &Path) -> Vec<Entry> {
    let mut photos: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()).is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg")))
        .map(|path| (taken(&path), path))
        .collect();
    photos.sort();
    photos.into_iter().map(|(date, path)| Entry {
        caption: date.map_or_else(String::new, |d| d.format("%B %Y").to_string()),
        path,
    }).collect()
}

/// Scan $FRAME_PHOTOS (fallback: assets/photos) and give the UI one empty row per photo.
pub fn init(app: &AppWindow) {
    let directory = dir();
    let bundled = bundled_dir();
    let weak = app.as_weak();
    std::thread::spawn(move || {
        let mut entries = scan(&directory);
        if entries.is_empty() && directory != bundled { entries = scan(&bundled); }
        if PLAYLIST.set(entries).is_err() { return; }
        let (published, publication) = std::sync::mpsc::sync_channel(1);
        let ui = weak.clone();
        if slint::invoke_from_event_loop(move || {
            let Some(app) = ui.upgrade() else { return };
            let rows: Vec<Photo> = PLAYLIST.get().unwrap().iter()
                .map(|e| Photo { title: e.caption.as_str().into(), ..Default::default() }).collect();
            let frame = app.global::<Frame>();
            frame.set_photos(ModelRc::new(VecModel::from(rows)));
            frame.set_photos_loading(false);
            demand(&app);
            let _ = published.send(());
        }).is_err() || publication.recv().is_err() { return; }
        work(weak);
    });
}

const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RESIZE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_THUMBS: usize = 64;

fn memory_limit() -> image::ImageError {
    image::ImageError::Limits(image::error::LimitError::from_kind(image::error::LimitErrorKind::InsufficientMemory))
}

fn photo_decoder(path: &Path) -> image::ImageResult<impl ImageDecoder> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_FILE_BYTES { return Err(memory_limit()); }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES { return Err(memory_limit()); }
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_SOURCE_BYTES);
    limits.max_image_width = Some(16384);
    limits.max_image_height = Some(16384);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Jpeg);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    if decoder.total_bytes() > MAX_SOURCE_BYTES || u64::from(width) * u64::from(height) * 3 > MAX_SOURCE_BYTES { return Err(memory_limit()); }
    Ok(decoder)
}

fn decode_oriented(path: &Path) -> image::ImageResult<RgbImage> {
    let mut decoder = photo_decoder(path)?;
    let orientation = decoder.orientation()?;
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(image.into_rgb8())
}

#[cfg(test)]
fn cover(path: &Path, w: u32, h: u32, current: impl Fn() -> bool) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
    if w == 0 || h == 0 || !current() { return None; }
    let img = decode_oriented(path).map_err(|e| eprintln!("[photos] {}: {e}", path.display())).ok()?;
    fit(&img, w, h, current)
}

fn fit(img: &RgbImage, w: u32, h: u32, current: impl Fn() -> bool) -> Option<SharedPixelBuffer<Rgb8Pixel>> {
    if w == 0 || h == 0 || w > 4096 || h > 4096
        || u64::from(w) * u64::from(h) * 3 > MAX_OUTPUT_BYTES || !current() { return None; }
    let scale = (w as f32 / img.width() as f32).max(h as f32 / img.height() as f32);
    let (sw, sh) = ((img.width() as f32 * scale).ceil() as u32, (img.height() as f32 * scale).ceil() as u32);
    if u64::from(sw) * u64::from(sh) * 3 > MAX_RESIZE_BYTES {
        eprintln!("[photos] cover resize exceeds the pixel budget");
        return None;
    }
    // Keep resize-before-crop: cropping first changes Triangle edge samples.
    let resized;
    let scaled = if (sw, sh) == img.dimensions() { img }
        else { resized = imageops::resize(img, sw.max(w), sh.max(h), FilterType::Triangle); &resized };
    if !current() { return None; }
    let (x, y) = ((scaled.width() - w) / 2, (scaled.height() - h) / 2);
    let mut output = SharedPixelBuffer::<Rgb8Pixel>::new(w, h);
    for (row, dest) in output.make_mut_bytes().chunks_exact_mut(w as usize * 3).enumerate() {
        let start = ((y as usize + row) * scaled.width() as usize + x as usize) * 3;
        dest.copy_from_slice(&scaled.as_raw()[start..start + dest.len()]);
    }
    Some(output)
}

/// Photos-tab backdrop: the UI scales this tiny image up, so a light blur finishes the look.
fn soften(photo: SharedPixelBuffer<Rgb8Pixel>) -> SharedPixelBuffer<Rgb8Pixel> {
    let (w, h) = (photo.width(), photo.height());
    let image = RgbImage::from_raw(w, h, photo.as_bytes().to_vec()).expect("buffer matches its size");
    SharedPixelBuffer::clone_from_slice(imageops::blur(&image, 2.0).as_raw(), w, h)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Kind {
    /// Home photo card, with the caption scrim baked in when the frame asks.
    Card,
    /// Window-sized; resident only while the screensaver is up.
    Full,
    Blur,
    Thumb,
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct Card {
    size: PhotoSize,
    bake: bool,
    generation: u64,
}

/// What the UI needs resident right now. Everything else is released.
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

/// Photo indexes kept decoded around `current`, most urgent first: the photo
/// shown, the next, the one just left (still crossfading), one more to prefetch.
fn window(current: usize, count: usize) -> Vec<usize> {
    let mut out = Vec::with_capacity(4);
    for offset in [0, 1, count.saturating_sub(1), 2] {
        let index = (current % count.max(1) + offset) % count.max(1);
        if index < count && !out.contains(&index) { out.push(index); }
    }
    out
}

impl Want {
    /// Every decoded image that may be resident, in load order.
    fn slots(&self, count: usize) -> Vec<(usize, Kind, PhotoSize)> {
        let mut card_slots = Vec::new();
        let mut card_indexes = self.card_displayed.clone();
        if !self.full {
            for index in window(self.current, count) {
                if !card_indexes.contains(&index) { card_indexes.push(index); }
            }
        }
        for index in card_indexes.into_iter().filter(|&index| index < count) {
            if let Some(Card { size: (w, h, scrim), bake, .. }) = self.card {
                card_slots.push((index, Kind::Card, (w, h, if bake { scrim } else { 0 })));
            }
        }
        let mut screen_slots = Vec::new();
        let mut screen_indexes = vec![self.current];
        for &index in &self.full_displayed {
            if !screen_indexes.contains(&index) { screen_indexes.push(index); }
        }
        for index in window(self.current, count) {
            if !screen_indexes.contains(&index) { screen_indexes.push(index); }
        }
        for index in screen_indexes.into_iter().filter(|&index| index < count) {
            if let Some((w, h)) = self.screen {
                if self.full {
                    screen_slots.push((index, Kind::Full, (w, h, 0)));
                } else {
                    screen_slots.push((index, Kind::Blur, ((w / BLUR_DIVISOR).max(1), (h / BLUR_DIVISOR).max(1), 0)));
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
        if !self.full {
            slots.extend(self.thumbs.clone().filter(|&i| i < count).take(MAX_THUMBS).map(|i| (i, Kind::Thumb, (THUMB, THUMB, 0))));
        }
        slots
    }
}

struct Job {
    index: usize,
    specs: Vec<(Kind, PhotoSize)>,
}

struct Step {
    evict: Vec<(usize, Kind)>,
    job: Option<Job>,
    card_done: Option<Card>,
}

struct Shared {
    want: Want,
    have: BTreeMap<(usize, Kind), PhotoSize>,
    generation: u64,
    /// Card batch opened by `load_scaled`; closed once its window is resident.
    card_open: bool,
}

type Buffer = SharedPixelBuffer<Rgb8Pixel>;

impl Shared {
    const fn new() -> Self {
        Self {
            want: Want { current: 0, card_displayed: Vec::new(), full_displayed: Vec::new(), card: None, screen: None, full: false, thumbs: 0..0 },
            have: BTreeMap::new(),
            generation: 0,
            card_open: false,
        }
    }

    fn accepts(&self, generation: u64, index: usize, kind: Kind, spec: PhotoSize, count: usize) -> bool {
        self.generation == generation && self.want.slots(count).contains(&(index, kind, spec))
    }

    /// Release what is no longer wanted, then pick the next photo with missing images.
    fn step(&mut self, count: usize) -> Step {
        let slots = self.want.slots(count);
        let evict: Vec<_> = self.have.keys().copied()
            .filter(|&(i, k)| !slots.iter().any(|&(si, sk, _)| (si, sk) == (i, k))).collect();
        let missing: Vec<_> = slots.iter().filter(|&&(i, k, spec)| self.have.get(&(i, k)) != Some(&spec)).collect();
        let job = missing.first().map(|&&(index, ..)| Job {
            index,
            specs: missing.iter().filter(|s| s.0 == index).map(|&&(_, k, spec)| (k, spec)).collect(),
        });
        let card_missing = missing.iter().any(|s| s.1 == Kind::Card);
        let card_done = if self.card_open && !card_missing { self.want.card } else { None };
        Step { evict, job, card_done }
    }
}

static SHARED: Mutex<Shared> = Mutex::new(Shared::new());
static DEMAND_CHANGED: Condvar = Condvar::new();
static LOAD_STATE: Mutex<PhotoLoadState> = Mutex::new(PhotoLoadState::new());

fn update(change: impl FnOnce(&mut Shared) -> bool) {
    let mut shared = SHARED.lock().unwrap();
    if change(&mut shared) {
        shared.generation = shared.generation.wrapping_add(1);
        DEMAND_CHANGED.notify_one();
    }
}

/// Card pixel size changed (debounced by the caller): decode the window at that size.
pub fn load_scaled(_weak: slint::Weak<AppWindow>, size: PhotoSize, bake: bool) {
    let (generation, requested) = {
        let mut state = LOAD_STATE.lock().unwrap();
        let requested = state.request_mode(size, bake).is_some();
        (state.generation, requested)
    };
    update(|s| {
        let card = Some(Card { size, bake, generation });
        if !requested && s.want.card == card { return false; }
        // A return to the completed size still cancels the worker's outdated resize demand.
        s.want.card = card;
        s.card_open = true;
        true
    });
}

/// Re-read what the UI shows: photo, tab, screensaver, grid rows, window size.
pub fn demand(app: &AppWindow) {
    let f = app.global::<Frame>();
    let saver = app.global::<Saver>();
    let rows = |v: i32| v.max(0) as usize;
    let thumbs = if f.get_active_tab() == 2 { rows(saver.get_grid_first())..rows(saver.get_grid_end()) } else { 0..0 };
    update(|s| {
        let pair = |current: i32, previous: i32| [current, previous].into_iter().filter(|&index| index >= 0).map(|index| index as usize).collect();
        let (w, h) = (saver.get_pixel_width().max(0) as u32, saver.get_pixel_height().max(0) as u32);
        let want = Want {
            current: rows(f.get_photo_index()),
            card_displayed: pair(saver.get_card_current(), saver.get_card_previous()),
            full_displayed: pair(saver.get_full_current(), saver.get_full_previous()),
            card: s.want.card,
            screen: (w > 0 && h > 0).then_some((w, h)),
            full: saver.get_active(),
            thumbs,
        };
        if s.want == want { return false; }
        s.want = want;
        true
    });
}

/// Decode `job` once and render each wanted size from it. `None` marks a failure.
fn render(path: &Path, job: &Job, wanted: impl Fn(Kind, PhotoSize) -> bool) -> Vec<(Kind, PhotoSize, Option<Buffer>)> {
    let mut decoded = None;
    let mut out = Vec::new();
    for &(kind, spec @ (w, h, scrim)) in &job.specs {
        if !wanted(kind, spec) { continue; }
        let key = (kind != Kind::Blur).then(|| ThumbnailKey::read(path, spec, scrim > 0).ok()).flatten();
        let cached = key.as_ref().and_then(|key| THUMBNAILS.lock().unwrap().get(key));
        let buf = cached.or_else(|| {
            let image = decoded.get_or_insert_with(|| {
                decode_oriented(path).map_err(|e| eprintln!("[photos] {}: {e}", path.display())).ok()
            }).as_ref()?;
            let mut buf = fit(image, w, h, || wanted(kind, spec))?;
            if kind == Kind::Blur { buf = soften(buf); }
            if scrim > 0 {
                match raster_scrim(w, scrim.min(h)) {
                    Ok(scrim) => composite_scrim(&mut buf, &scrim),
                    Err(error) => { eprintln!("[photos] {error}; keeping the previous photo"); return None; }
                }
            }
            if let Some(key) = key {
                if ThumbnailKey::read(path, spec, scrim > 0).ok().as_ref() == Some(&key) {
                    THUMBNAILS.lock().unwrap().insert(key, buf.clone());
                }
            }
            Some(buf)
        });
        out.push((kind, spec, buf));
    }
    out
}

fn work(weak: slint::Weak<AppWindow>) {
    let playlist = PLAYLIST.get().map_or(&[][..], Vec::as_slice);
    loop {
        let (step, card, generation) = {
            let mut shared = SHARED.lock().unwrap();
            loop {
                let step = shared.step(playlist.len());
                if step.job.is_some() || !step.evict.is_empty() || step.card_done.is_some() {
                    break (step, shared.want.card, shared.generation);
                }
                shared = DEMAND_CHANGED.wait(shared).unwrap();
            }
        };
        let mut updates = Vec::new();
        if let Some(job) = &step.job {
            let rendered = render(&playlist[job.index].path, job, |kind, spec| {
                let shared = SHARED.lock().unwrap();
                shared.accepts(generation, job.index, kind, spec, playlist.len())
            });
            for (kind, spec, buf) in rendered {
                updates.push((job.index, kind, spec, buf));
            }
        }
        let (acknowledge, acknowledged) = std::sync::mpsc::sync_channel::<()>(1);
        if slint::invoke_from_event_loop({
            let weak = weak.clone();
            move || {
                let _acknowledge = acknowledge;
                let Some(app) = weak.upgrade() else { return };
                // Refresh from the actual scene before admitting a queued decode result.
                demand(&app);
                let mut shared = SHARED.lock().unwrap();
                if shared.generation != generation { return; }
                let slots = shared.want.slots(playlist.len());
                let mut admitted = Vec::new();
                for (index, kind) in step.evict {
                    if !slots.iter().any(|&(i, k, _)| (i, k) == (index, kind)) {
                        shared.have.remove(&(index, kind));
                        admitted.push((index, kind, None));
                    }
                }
                for (index, kind, spec, buf) in updates {
                    if slots.contains(&(index, kind, spec)) {
                        // Failed slots are remembered until their demand changes, avoiding a retry loop.
                        shared.have.insert((index, kind), spec);
                        if buf.is_some() { admitted.push((index, kind, buf)); }
                    }
                }
                if step.card_done == shared.want.card && step.card_done.is_some() { shared.card_open = false; }
                drop(shared);
                let f = app.global::<Frame>();
                let model = f.get_photos();
                for (index, kind, buf) in admitted {
                    let Some(mut photo) = model.row_data(index) else { continue };
                    let published = buf.is_some();
                    let image = buf.map(slint::Image::from_rgb8).unwrap_or_default();
                    match kind {
                        Kind::Card => {
                            if let (true, Some(Card { size, bake, generation })) = (published, card) {
                                f.set_photo_scrim_baked(bake);
                                LOAD_STATE.lock().unwrap().ready(generation, size);
                            }
                            photo.image = image;
                        }
                        Kind::Full => photo.full = image,
                        Kind::Blur => photo.blur = image,
                        Kind::Thumb => photo.thumb = image,
                    }
                    model.set_row_data(index, photo);
                }
                if let Some(Card { generation, .. }) = step.card_done { LOAD_STATE.lock().unwrap().finish(generation); }
            }
        }).is_err() { return; }
        // Dropping the callback's sender acknowledges both publication and rejection.
        let _ = acknowledged.recv();
    }
}

#[cfg(test)]
mod tests {
    use super::{taken, composite_scrim, cover, decode_oriented, exif_date, fit, raster_scrim, scan, window, Card, Kind, Shared, ThumbnailCache, ThumbnailKey, Want, MAX_THUMBS};
    use image::{imageops, Rgb, RgbImage};
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};
    use slint::{Rgb8Pixel, SharedPixelBuffer};

    struct PhotoFixture(PathBuf);

    impl PhotoFixture {
        fn new() -> Self {
            let unique = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos();
            let dir = std::env::temp_dir().join(format!("photoframe-photos-{}-{unique}", std::process::id()));
            std::fs::create_dir(&dir).unwrap();
            let image = RgbImage::from_fn(48, 32, |x, y| Rgb([(x * 5) as u8, (y * 7) as u8, 83]));
            let file = std::fs::File::create(dir.join("photo.jpg")).unwrap();
            image::codecs::jpeg::JpegEncoder::new_with_quality(file, 100).encode_image(&image).unwrap();
            Self(dir)
        }

        fn photo(&self) -> PathBuf { self.0.join("photo.jpg") }
    }

    impl Drop for PhotoFixture {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    #[test]
    fn jpeg_exif_rotation_and_mirroring_are_applied_before_cover() {
        let fixture = PhotoFixture::new();
        let original = decode_oriented(&fixture.photo()).unwrap();
        let jpeg = std::fs::read(fixture.photo()).unwrap();
        for orientation in [2u8, 6] {
            let mut exif = b"Exif\0\0".to_vec();
            exif.extend_from_slice(&[b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0,
                0x12, 1, 3, 0, 1, 0, 0, 0, orientation, 0, 0, 0, 0, 0, 0, 0]);
            let mut tagged = jpeg[..2].to_vec();
            tagged.extend_from_slice(&[0xff, 0xe1]);
            tagged.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
            tagged.extend_from_slice(&exif);
            tagged.extend_from_slice(&jpeg[2..]);
            let path = fixture.0.join(format!("orientation-{orientation}.jpg"));
            std::fs::write(&path, tagged).unwrap();
            let image = decode_oriented(&path).unwrap();
            assert_eq!(image.dimensions(), if orientation == 6 { (32, 48) } else { (48, 32) });
            for (x, y, pixel) in original.enumerate_pixels() {
                let target = if orientation == 6 { (31 - y, x) } else { (47 - x, y) };
                assert_eq!(image.get_pixel(target.0, target.1), pixel);
            }
            let thumbnail = cover(&path, image.width(), image.height(), || true).unwrap();
            assert_eq!(thumbnail.as_bytes(), image.as_raw());
        }
    }

    #[test]
    fn cover_keeps_original_center_crop_pixels_at_exact_target_sizes() {
        let fixture = PhotoFixture::new();
        let image = decode_oriented(&fixture.photo()).unwrap();
        for (w, h) in [(48, 32), (24, 24), (14, 42), (47, 30)] {
            let scale = (w as f32 / image.width() as f32).max(h as f32 / image.height() as f32);
            let scaled = imageops::resize(&image, (image.width() as f32 * scale).ceil() as u32,
                (image.height() as f32 * scale).ceil() as u32, imageops::FilterType::Triangle);
            let expected = imageops::crop_imm(&scaled, (scaled.width() - w) / 2, (scaled.height() - h) / 2, w, h).to_image();
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
        assert_eq!(ThumbnailKey::read(&fixture.0.join(".").join("photo.jpg"), first.size, false).unwrap(), first);
        let mut cache = ThumbnailCache::new();
        let image = cover(&path, 24, 24, || true).unwrap();
        cache.insert(first.clone(), image.clone());
        let second = ThumbnailKey::read(&path, (14, 42, 10), false).unwrap();
        cache.insert(second.clone(), cover(&path, 14, 42, || true).unwrap());
        assert_eq!(cache.get(&first).unwrap().as_bytes().as_ptr(), image.as_bytes().as_ptr());
        assert!(cache.get(&ThumbnailKey::read(&path, first.size, true).unwrap()).is_none());
        assert!(cache.get(&ThumbnailKey::read(&path, (24, 24, 11), false).unwrap()).is_none());
        std::fs::File::options().write(true).open(&path).unwrap().set_times(
            std::fs::FileTimes::new().set_modified(first.modified + Duration::from_secs(2))).unwrap();
        let updated = ThumbnailKey::read(&path, first.size, false).unwrap();
        assert_ne!(first, updated);
        assert!(cache.get(&updated).is_none());
        cache.insert(updated.clone(), image.clone());
        assert!(cache.get(&first).is_none());
        assert!(cache.get(&second).is_none());
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(&[0]).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_times(
            std::fs::FileTimes::new().set_modified(updated.modified)).unwrap();
        assert!(cache.get(&ThumbnailKey::read(&path, first.size, false).unwrap()).is_none());
    }

    #[test]
    fn thumbnail_cache_evicts_least_recent_entries_and_bounds_pixel_memory() {
        let fixture = PhotoFixture::new();
        let mut cache = ThumbnailCache::new();
        let first = ThumbnailKey::read(&fixture.photo(), (1, 1, 0), false).unwrap();
        for w in 1..=16 {
            let key = ThumbnailKey::read(&fixture.photo(), (w, 1, 0), false).unwrap();
            cache.insert(key, SharedPixelBuffer::new(w, 1));
        }
        assert!(cache.get(&first).is_some());
        cache.insert(ThumbnailKey::read(&fixture.photo(), (17, 1, 0), false).unwrap(), SharedPixelBuffer::new(17, 1));
        assert!(cache.get(&first).is_some());
        assert!(cache.get(&ThumbnailKey::read(&fixture.photo(), (2, 1, 0), false).unwrap()).is_none());
        for height in 1024..1030 {
            cache.insert(ThumbnailKey::read(&fixture.photo(), (1024, height, 0), false).unwrap(), SharedPixelBuffer::new(1024, height));
            assert!(cache.bytes <= ThumbnailCache::MAX_BYTES);
            assert!(cache.entries.len() <= ThumbnailCache::MAX_ENTRIES);
        }
        assert!(cache.get(&first).is_none());
    }

    #[test]
    fn canonical_svg_preserves_top_pixels_and_darkens_only_bottom_strip() {
        let scrim = raster_scrim(388, 180).unwrap();
        let white = Rgb8Pixel { r: 255, g: 255, b: 255 };
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
        photo.make_mut_slice().fill(Rgb8Pixel { r: 200, g: 100, b: 50 });
        let scrim = image::RgbaImage::from_raw(2, 1, vec![20, 40, 60, 0, 20, 40, 60, 128]).unwrap();
        composite_scrim(&mut photo, &scrim);
        assert_eq!(photo.as_slice()[0], Rgb8Pixel { r: 200, g: 100, b: 50 });
        assert_eq!(photo.as_slice()[1], Rgb8Pixel { r: 110, g: 70, b: 55 });
    }

    #[test]
    fn caption_is_month_and_year_from_exif_date_taken() {
        let date = b"2023:03:14 10:20:30\0";
        let mut tiff = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        // IFD0: one entry, Exif IFD pointer -> 26
        tiff.extend_from_slice(&[1, 0, 0x69, 0x87, 4, 0, 1, 0, 0, 0, 26, 0, 0, 0, 0, 0, 0, 0]);
        // Exif IFD: one entry, DateTimeOriginal (ASCII, 20) -> 44
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
        assert_eq!(scan(&fixture.0).iter().map(|e| e.caption.as_str()).collect::<Vec<_>>(), ["", "March 2023"]);
    }

    /// Peak bytes of decoded pixels while visiting every photo in order.
    fn resident_bytes(count: usize) -> usize {
        let mut shared = Shared::new();
        shared.want = Want {
            card: Some(Card { size: (388, 280, 180), bake: true, generation: 1 }),
            screen: Some((800, 1280)),
            full: true,
            thumbs: 0..21,
            ..Want::default()
        };
        let mut peak = 0;
        for current in 0..count {
            shared.want.current = current;
            loop {
                let step = shared.step(count);
                for key in step.evict { shared.have.remove(&key); }
                let Some(job) = step.job else { break };
                for (kind, spec) in job.specs { shared.have.insert((job.index, kind), spec); }
                peak = peak.max(shared.have.values().map(|&(w, h, _)| w as usize * h as usize * 3).sum());
            }
        }
        peak
    }

    #[test]
    fn resident_pixels_stay_bounded_as_the_photo_count_grows() {
        assert_eq!(window(0, 66), [0, 1, 65, 2]);
        assert_eq!(window(65, 66), [65, 0, 64, 1]);
        assert_eq!(window(0, 2), [0, 1]);
        assert!(window(0, 0).is_empty());
        let small = resident_bytes(30);
        let large = resident_bytes(2000);
        // 4 photos x (card + screensaver frame + backdrop) + 21 grid thumbnails
        let limit = 4 * (388 * 280 + 800 * 1280 + 100 * 160) * 3 + 21 * 270 * 270 * 3;
        assert_eq!(small, large);
        assert!(large <= limit, "{large} > {limit}");
    }

    #[test]
    fn rapid_grid_jump_keeps_both_images_actually_in_transition() {
        let want = Want {
            current: 40,
            card_displayed: vec![1, 0],
            full_displayed: vec![9, 8],
            card: Some(Card { size: (388, 280, 180), bake: true, generation: 1 }),
            screen: Some((800, 1280)),
            full: true,
            thumbs: 0..2000,
        };
        let slots = want.slots(2000);
        for index in [0, 1] { assert!(slots.contains(&(index, Kind::Card, (388, 280, 180)))); }
        for index in [8, 9] { assert!(slots.contains(&(index, Kind::Full, (800, 1280, 0)))); }
        assert_eq!(slots.iter().filter(|s| s.1 == Kind::Thumb).count(), 0);
        assert_eq!(slots.iter().filter(|s| s.1 == Kind::Card).count(), 2);
        assert_eq!(slots.iter().filter(|s| s.1 == Kind::Full).count(), 6);
    }

    #[test]
    fn slideshow_loads_requested_frame_before_hidden_cards_and_grid() {
        let mut shared = Shared::new();
        shared.want = Want {
            current: 0,
            full_displayed: vec![65],
            card: Some(Card { size: (388, 280, 180), bake: true, generation: 1 }),
            screen: Some((800, 1280)),
            full: true,
            thumbs: 0..21,
            ..Want::default()
        };
        let step = shared.step(66);
        let job = step.job.unwrap();
        assert_eq!(job.index, 0);
        assert_eq!(job.specs, [(Kind::Full, (800, 1280, 0))]);
        let slots = shared.want.slots(66);
        assert!(slots.contains(&(65, Kind::Full, (800, 1280, 0))));
        assert!(!slots.iter().any(|slot| matches!(slot.1, Kind::Card | Kind::Blur)));
        shared.want.full = false;
        let slots = shared.want.slots(66);
        assert_eq!(slots[0], (0, Kind::Card, (388, 280, 180)));
        assert_eq!(slots.iter().filter(|slot| slot.1 == Kind::Thumb).count(), 21);
        shared.want.thumbs = 0..2000;
        assert_eq!(shared.want.slots(2000).iter().filter(|slot| slot.1 == Kind::Thumb).count(), MAX_THUMBS);
    }

    #[test]
    fn queued_decode_requires_current_demand_generation_and_exact_slot_size() {
        let mut shared = Shared::new();
        shared.generation = 3;
        shared.want.current = 4;
        shared.want.card = Some(Card { size: (388, 280, 180), bake: true, generation: 1 });
        assert!(shared.accepts(3, 4, Kind::Card, (388, 280, 180), 20));
        shared.generation = 4;
        assert!(!shared.accepts(3, 4, Kind::Card, (388, 280, 180), 20));
        shared.want.card.as_mut().unwrap().bake = false;
        assert!(!shared.accepts(4, 4, Kind::Card, (388, 280, 180), 20));
        assert!(shared.accepts(4, 4, Kind::Card, (388, 280, 0), 20));
        shared.want.current = 15;
        assert!(!shared.accepts(4, 4, Kind::Card, (388, 280, 0), 20));
    }

    #[test]
    fn pathological_panorama_and_oversized_target_fail_before_resize_allocation() {
        let image = RgbImage::new(10000, 100);
        assert!(fit(&image, 800, 1280, || true).is_none());
        assert!(fit(&image, 10000, 10000, || true).is_none());
    }
}
