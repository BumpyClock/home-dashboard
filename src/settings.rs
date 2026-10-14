use slint::Color;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, OnceLock};
use std::time::Duration;

use crate::Palette;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub theme: usize,
    pub display_font: usize,
    pub body_font: usize,
    pub scale: usize,
    pub clock24: bool,
    pub motion: usize,
    pub photo_slide: bool,
    pub screensaver_idle: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: 0,
            display_font: 0,
            body_font: 0,
            scale: 0,
            clock24: true,
            motion: 0,
            photo_slide: true,
            screensaver_idle: 2,
        }
    }
}

pub const SCALES: [f32; 3] = [1.0, 1.12, 1.25];
pub const SCREENSAVER_IDLE_SECS: [u64; 4] = [0, 60, 120, 300];

pub const DISPLAY_FONTS: [(&str, &str); 4] = [
    ("Instrument Serif", "Instrument Serif"),
    ("DM Serif Display", "DM Serif Display"),
    ("Young Serif", "Young Serif"),
    ("Same as body", ""),
];

pub const BODY_FONTS: [(&str, &str); 2] = [
    ("Inter", "Inter"),
    ("Atkinson Hyperlegible", "Atkinson Hyperlegible"),
];

fn c(hex: u32) -> Color {
    Color::from_rgb_u8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

pub fn themes() -> Vec<(&'static str, Palette)> {
    vec![
        (
            "Linen",
            Palette {
                bg: c(0xF2EEE6),
                surface: c(0xFBF9F5),
                card_hi: c(0xFFFFFF),
                ink: c(0x1D1C1A),
                muted: c(0x6F6A61),
                line: c(0xE3DDD1),
                accent: c(0xC9532D),
                rail: c(0x1D1C1A),
                rail_fg: c(0xA9A399),
                weather_bg: c(0x1D1C1A),
                weather_fg: c(0xFBF9F5),
                weather_muted: c(0xA9A399),
                weather_sub: c(0xD9D3C8),
                weather_rule: c(0x3A3834),
                weather_icon: c(0xE9A88F),
                dot_empty: c(0xE9E4DA),
            },
        ),
        (
            "Sage",
            Palette {
                bg: c(0xE8ECE4),
                surface: c(0xF5F7F2),
                card_hi: c(0xFFFFFF),
                ink: c(0x1B2620),
                muted: c(0x5D6A60),
                line: c(0xD3DCCF),
                accent: c(0x3E7B55),
                rail: c(0x1E3328),
                rail_fg: c(0xA3B7A9),
                weather_bg: c(0x1E3328),
                weather_fg: c(0xF5F7F2),
                weather_muted: c(0xA3B7A9),
                weather_sub: c(0xCFDBD2),
                weather_rule: c(0x34493D),
                weather_icon: c(0xE6C27A),
                dot_empty: c(0xDCE3D8),
            },
        ),
        (
            "Harbor",
            Palette {
                bg: c(0xE7ECF1),
                surface: c(0xF5F7FA),
                card_hi: c(0xFFFFFF),
                ink: c(0x15202B),
                muted: c(0x5A6676),
                line: c(0xD2DAE3),
                accent: c(0x2D6CB0),
                rail: c(0x14243A),
                rail_fg: c(0x9AABBF),
                weather_bg: c(0x14243A),
                weather_fg: c(0xF5F7FA),
                weather_muted: c(0x9AABBF),
                weather_sub: c(0xC9D5E2),
                weather_rule: c(0x2A3B52),
                weather_icon: c(0xF2B880),
                dot_empty: c(0xDCE3EA),
            },
        ),
        (
            "Paper",
            Palette {
                bg: c(0xF6F5F2),
                surface: c(0xFFFFFF),
                card_hi: c(0xFFF1E6),
                ink: c(0x000000),
                muted: c(0x4A4A48),
                line: c(0xD9D7D2),
                accent: c(0xC2410C),
                rail: c(0x000000),
                rail_fg: c(0xB5B3AE),
                weather_bg: c(0x000000),
                weather_fg: c(0xFFFFFF),
                weather_muted: c(0xB5B3AE),
                weather_sub: c(0xE3E1DC),
                weather_rule: c(0x333333),
                weather_icon: c(0xFDBA74),
                dot_empty: c(0xE4E2DD),
            },
        ),
        (
            "Night",
            Palette {
                bg: c(0x121110),
                surface: c(0x1C1A18),
                card_hi: c(0x2A2622),
                ink: c(0xEEE9E0),
                muted: c(0x9B948A),
                line: c(0x2D2A26),
                accent: c(0xE58A62),
                rail: c(0x0A0908),
                rail_fg: c(0x7D776E),
                weather_bg: c(0x1C1A18),
                weather_fg: c(0xEEE9E0),
                weather_muted: c(0x9B948A),
                weather_sub: c(0xC9C2B7),
                weather_rule: c(0x34302B),
                weather_icon: c(0xE9A88F),
                dot_empty: c(0x2D2A26),
            },
        ),
    ]
}

fn path() -> PathBuf {
    if let Ok(p) = std::env::var("FRAME_SETTINGS") {
        return p.into();
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config/family-frame/settings.json")
}

#[derive(PartialEq, Eq)]
struct SaveSnapshot {
    path: PathBuf,
    json: String,
}

enum SaveCommand {
    Save(SaveSnapshot),
    Flush(mpsc::Sender<io::Result<()>>),
}

static WRITER: OnceLock<mpsc::Sender<SaveCommand>> = OnceLock::new();
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(250);
const SAVE_RETRY: Duration = Duration::from_secs(15);

fn atomic_write(path: &Path, json: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "settings path has no filename")
    })?;
    let (temp, mut file) = loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = name.to_os_string();
        temp_name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
        let temp = parent.join(temp_name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp) {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = (|| {
        #[cfg(unix)]
        if let Ok(metadata) = std::fs::metadata(path) {
            std::fs::set_permissions(&temp, metadata.permissions())?;
        }
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)?;
        #[cfg(unix)]
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn commit_pending(
    pending: &mut Option<SaveSnapshot>,
    saved: &mut Option<SaveSnapshot>,
    write: &mut impl FnMut(&SaveSnapshot) -> io::Result<()>,
) -> io::Result<()> {
    let Some(snapshot) = pending.as_ref() else {
        return Ok(());
    };
    if saved.as_ref() != Some(snapshot) {
        write(snapshot)?;
    }
    *saved = pending.take();
    Ok(())
}

fn run_writer(
    receiver: mpsc::Receiver<SaveCommand>,
    mut write: impl FnMut(&SaveSnapshot) -> io::Result<()>,
) {
    let (mut pending, mut saved) = (None, None);
    let mut delay = SAVE_DEBOUNCE;
    loop {
        let command = if pending.is_some() {
            receiver.recv_timeout(delay)
        } else {
            receiver
                .recv()
                .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
        };
        match command {
            Ok(SaveCommand::Save(snapshot)) => {
                pending = Some(snapshot);
                delay = SAVE_DEBOUNCE;
            }
            Ok(SaveCommand::Flush(reply)) => {
                let _ = reply.send(commit_pending(&mut pending, &mut saved, &mut write));
                delay = SAVE_RETRY;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Err(error) = commit_pending(&mut pending, &mut saved, &mut write) {
                    eprintln!("[settings] could not save; will retry: {error}");
                }
                delay = SAVE_RETRY;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Err(error) = commit_pending(&mut pending, &mut saved, &mut write) {
                    eprintln!("[settings] final save failed: {error}");
                }
                return;
            }
        }
    }
}

pub struct SaveGuard;

pub fn save_guard() -> SaveGuard {
    SaveGuard
}

pub fn flush_pending_saves() -> io::Result<()> {
    let Some(writer) = WRITER.get() else {
        return Ok(());
    };
    let (reply, result) = mpsc::channel();
    writer
        .send(SaveCommand::Flush(reply))
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "settings writer stopped"))?;
    result
        .recv()
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "settings writer stopped"))?
}

impl Drop for SaveGuard {
    fn drop(&mut self) {
        if let Err(error) = flush_pending_saves() {
            eprintln!("[settings] final save failed: {error}");
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        let d = Self::default();
        let Ok(text) = std::fs::read_to_string(path()) else {
            return d;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            return d;
        };
        let idx = |k: &str, max: usize, def: usize| {
            v[k].as_u64()
                .map(|n| (n as usize).min(max - 1))
                .unwrap_or(def)
        };
        Self {
            theme: idx("theme", themes().len(), d.theme),
            display_font: idx("display_font", DISPLAY_FONTS.len(), d.display_font),
            body_font: idx("body_font", BODY_FONTS.len(), d.body_font),
            scale: idx("scale", SCALES.len(), d.scale),
            clock24: v["clock24"].as_bool().unwrap_or(d.clock24),
            photo_slide: v["photo_slide"].as_bool().unwrap_or(d.photo_slide),
            screensaver_idle: idx(
                "screensaver_idle",
                SCREENSAVER_IDLE_SECS.len(),
                d.screensaver_idle,
            ),
            motion: v["motion"]
                .as_u64()
                .map(|m| (m as usize).min(2))
                .or_else(|| v["motion"].as_bool().map(|on| if on { 0 } else { 2 }))
                .unwrap_or(d.motion),
        }
    }

    pub fn save(&self) {
        let json = serde_json::json!({
            "theme": self.theme, "display_font": self.display_font, "body_font": self.body_font,
            "scale": self.scale, "clock24": self.clock24, "motion": self.motion,
            "photo_slide": self.photo_slide, "screensaver_idle": self.screensaver_idle,
        });
        let snapshot = SaveSnapshot {
            path: path(),
            json: serde_json::to_string_pretty(&json).unwrap(),
        };
        let writer = WRITER.get_or_init(|| {
            let (sender, receiver) = mpsc::channel();
            std::thread::spawn(move || {
                run_writer(receiver, |snapshot| {
                    atomic_write(&snapshot.path, &snapshot.json)
                })
            });
            sender
        });
        if writer.send(SaveCommand::Save(snapshot)).is_err() {
            eprintln!("[settings] settings writer stopped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        atomic_write, commit_pending, run_writer, SaveCommand, SaveSnapshot, TEMP_SEQUENCE,
    };
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::sync::{mpsc, Arc, Mutex};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            loop {
                let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!(
                    "photoframe-settings-{}-{sequence}",
                    std::process::id()
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path.canonicalize().unwrap()),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("test directory: {error}"),
                }
            }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn atomic_save_replaces_existing_valid_json_without_leaving_temps() {
        let directory = TestDirectory::new();
        let target = directory.0.join("settings.json");
        atomic_write(&target, r#"{"theme":1}"#).unwrap();
        atomic_write(&target, r#"{"theme":2,"clock24":false}"#).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&target).unwrap()).unwrap();
        assert_eq!(saved["theme"], 2);
        assert_eq!(saved["clock24"], false);
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn failed_replacement_keeps_destination_and_removes_temp() {
        let directory = TestDirectory::new();
        let target = directory.0.join("settings.json");
        std::fs::create_dir(&target).unwrap();
        let previous = target.join("previous.json");
        std::fs::write(&previous, r#"{"theme":1}"#).unwrap();
        assert!(atomic_write(&target, r#"{"theme":2}"#).is_err());
        assert_eq!(
            std::fs::read_to_string(&previous).unwrap(),
            r#"{"theme":1}"#
        );
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn flush_coalesces_latest_snapshot_and_skips_unchanged_rewrite() {
        let directory = TestDirectory::new();
        let target = directory.0.join("settings.json");
        let (sender, receiver) = mpsc::channel();
        let (reply, result) = mpsc::channel();
        for theme in [1, 2] {
            sender
                .send(SaveCommand::Save(SaveSnapshot {
                    path: target.clone(),
                    json: format!("{{\"theme\":{theme}}}"),
                }))
                .unwrap();
        }
        sender.send(SaveCommand::Flush(reply)).unwrap();
        let writes = Arc::new(Mutex::new(Vec::new()));
        let observed = writes.clone();
        let worker = std::thread::spawn(move || {
            run_writer(receiver, |snapshot| {
                atomic_write(&snapshot.path, &snapshot.json)?;
                observed.lock().unwrap().push(snapshot.json.clone());
                Ok(())
            })
        });
        result.recv().unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), r#"{"theme":2}"#);
        let (reply, result) = mpsc::channel();
        sender
            .send(SaveCommand::Save(SaveSnapshot {
                path: target,
                json: r#"{"theme":2}"#.into(),
            }))
            .unwrap();
        sender.send(SaveCommand::Flush(reply)).unwrap();
        result.recv().unwrap().unwrap();
        drop(sender);
        worker.join().unwrap();
        assert_eq!(*writes.lock().unwrap(), vec![r#"{"theme":2}"#]);
    }

    #[test]
    fn failed_write_remains_pending_for_retry() {
        let mut pending = Some(SaveSnapshot {
            path: "test-only.json".into(),
            json: r#"{"theme":2}"#.into(),
        });
        let mut saved = None;
        let mut fail = |_: &SaveSnapshot| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "test failure",
            ))
        };
        assert!(commit_pending(&mut pending, &mut saved, &mut fail).is_err());
        assert!(saved.is_none());
        assert_eq!(pending.as_ref().unwrap().json, r#"{"theme":2}"#);
        let mut written = Vec::new();
        commit_pending(&mut pending, &mut saved, &mut |snapshot: &SaveSnapshot| {
            written.push(snapshot.json.clone());
            Ok(())
        })
        .unwrap();
        assert!(pending.is_none());
        assert_eq!(written, vec![r#"{"theme":2}"#]);
    }
}
