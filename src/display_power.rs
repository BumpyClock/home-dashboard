//! Backlight dimming keeps the screen on; rendering updates are not user activity.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Night {
    start: u16,
    end: u16,
    brightness: u8,
}

impl Night {
    fn contains(self, minute: u16) -> bool {
        if self.start < self.end {
            minute >= self.start && minute < self.end
        } else {
            minute >= self.start || minute < self.end
        }
    }
}

#[derive(Clone, Debug)]
struct Config {
    idle_after: Option<Duration>,
    day: u8,
    idle: u8,
    night: Option<Night>,
}

impl Config {
    fn parse(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let percent = |name, fallback| -> Result<u8, String> {
            match get(name) {
                None => Ok(fallback),
                Some(value) => value
                    .parse::<u8>()
                    .ok()
                    .filter(|p| (1..=100).contains(p))
                    .ok_or_else(|| format!("{name} must be an integer from 1 to 100")),
            }
        };
        let day = percent("FRAME_DAY_BRIGHTNESS", 65)?;
        let idle = percent("FRAME_IDLE_BRIGHTNESS", 20)?;
        let seconds = match get("FRAME_IDLE_DIM_SECONDS") {
            None => 180,
            Some(value) => value
                .parse::<u64>()
                .ok()
                .filter(|s| *s <= 86_400)
                .ok_or("FRAME_IDLE_DIM_SECONDS must be an integer from 0 to 86400")?,
        };
        let night = match (get("FRAME_NIGHT_START"), get("FRAME_NIGHT_END")) {
            (None, None) => None,
            (Some(start), Some(end)) => {
                let start = parse_time(&start).ok_or("FRAME_NIGHT_START must be HH:MM")?;
                let end = parse_time(&end).ok_or("FRAME_NIGHT_END must be HH:MM")?;
                if start == end {
                    return Err("night start and end must differ".into());
                }
                Some(Night {
                    start,
                    end,
                    brightness: percent("FRAME_NIGHT_BRIGHTNESS", 10)?,
                })
            }
            _ => return Err("both FRAME_NIGHT_START and FRAME_NIGHT_END are required".into()),
        };
        if idle > day || night.is_some_and(|n| n.brightness > day) {
            return Err("dim brightness must not exceed FRAME_DAY_BRIGHTNESS".into());
        }
        Ok(Self {
            idle_after: (seconds > 0).then(|| Duration::from_secs(seconds)),
            day,
            idle,
            night,
        })
    }

    fn brightness(&self, idle: bool, minute: u16) -> u8 {
        if !idle {
            return self.day;
        }
        self.night
            .filter(|n| n.contains(minute))
            .map_or(self.idle, |n| n.brightness)
    }
}

fn parse_time(value: &str) -> Option<u16> {
    let bytes = value.as_bytes();
    if bytes.len() != 5
        || bytes[2] != b':'
        || ![bytes[0], bytes[1], bytes[3], bytes[4]]
            .iter()
            .all(u8::is_ascii_digit)
    {
        return None;
    }
    let hour = (bytes[0] - b'0') as u16 * 10 + (bytes[1] - b'0') as u16;
    let minute = (bytes[3] - b'0') as u16 * 10 + (bytes[4] - b'0') as u16;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

fn next_night_boundary<T: chrono::TimeZone>(now: chrono::DateTime<T>, night: Night) -> Duration {
    let timezone = now.timezone();
    let mut next = None;
    for day in 0..=2 {
        let date = now.date_naive() + chrono::Duration::days(day);
        for minute in [night.start, night.end] {
            let civil = date
                .and_hms_opt(u32::from(minute / 60), u32::from(minute % 60), 0)
                .unwrap();
            // A skipped civil time takes effect at the first valid time after it.
            // During a clock rollback, consider both occurrences of the boundary.
            for adjustment in 0..=180 {
                let candidates = match timezone
                    .from_local_datetime(&(civil + chrono::Duration::minutes(adjustment)))
                {
                    chrono::LocalResult::None => continue,
                    chrono::LocalResult::Single(value) => vec![value],
                    chrono::LocalResult::Ambiguous(first, second) => vec![first, second],
                };
                for value in candidates {
                    if value > now && next.as_ref().is_none_or(|current| value < *current) {
                        next = Some(value);
                    }
                }
                break;
            }
        }
    }
    next.unwrap().signed_duration_since(now).to_std().unwrap()
}

#[derive(Clone, Debug)]
struct Backlight {
    brightness: PathBuf,
    maximum: u32,
}

impl Backlight {
    fn from_directory(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(io::Error::other("backlight path must be absolute"));
        }
        let directory = path.canonicalize()?;
        let brightness = directory.join("brightness").canonicalize()?;
        let maximum_file = directory.join("max_brightness").canonicalize()?;
        if brightness.parent() != Some(directory.as_path())
            || maximum_file.parent() != Some(directory.as_path())
        {
            return Err(io::Error::other(
                "backlight attribute escapes its directory",
            ));
        }
        let read = |file: &Path| -> io::Result<u32> {
            std::fs::read_to_string(file)?
                .trim()
                .parse()
                .map_err(io::Error::other)
        };
        let maximum = read(&maximum_file)?;
        let current = read(&brightness)?;
        if maximum == 0 || maximum > 1_000_000 || current > maximum {
            return Err(io::Error::other("invalid backlight brightness range"));
        }
        Ok(Self {
            brightness,
            maximum,
        })
    }

    fn discover(class: &Path, explicit: Option<&Path>) -> io::Result<Self> {
        if let Some(path) = explicit {
            return Self::from_directory(path);
        }
        let mut entries = std::fs::read_dir(class)?;
        let first = entries
            .next()
            .transpose()?
            .ok_or_else(|| io::Error::other("no backlight found"))?;
        if entries.next().is_some() {
            return Err(io::Error::other(
                "multiple backlights; set FRAME_BACKLIGHT_PATH",
            ));
        }
        Self::from_directory(&first.path())
    }

    fn raw(&self, percent: u8) -> u32 {
        ((u64::from(self.maximum) * u64::from(percent) + 50) / 100)
            .clamp(1, u64::from(self.maximum)) as u32
    }

    fn write(&self, value: u32) -> io::Result<()> {
        if value == 0 || value > self.maximum {
            return Err(io::Error::other("brightness outside range"));
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.brightness)?;
        write_brightness(&mut file, value)
    }
}

fn write_brightness(file: &mut impl io::Write, value: u32) -> io::Result<()> {
    // sysfs parses each write separately; a formatter's trailing newline can fail.
    let buffer = format!("{value}\n");
    file.write_all(buffer.as_bytes())
}

#[derive(Default)]
struct Requests {
    latest: Option<u32>,
    closing: bool,
}

#[derive(Default)]
struct SharedWriter {
    requests: Mutex<Requests>,
    changed: Condvar,
}

impl SharedWriter {
    fn request(&self, value: u32) {
        let mut requests = self.requests.lock().unwrap();
        if !requests.closing {
            requests.latest = Some(value);
            self.changed.notify_one();
        }
    }
}

struct Writer {
    shared: Arc<SharedWriter>,
    thread: Option<JoinHandle<()>>,
}

impl Writer {
    fn spawn(
        restore: u32,
        mut write: impl FnMut(u32) -> io::Result<()> + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new(SharedWriter::default());
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("frame-backlight".into())
            .spawn(move || {
                let mut last_written = None;
                loop {
                    let (value, closing) = {
                        let mut requests = worker.requests.lock().unwrap();
                        while requests.latest.is_none() && !requests.closing {
                            requests = worker.changed.wait(requests).unwrap();
                        }
                        (
                            if requests.closing {
                                restore
                            } else {
                                requests.latest.take().unwrap()
                            },
                            requests.closing,
                        )
                    };
                    if last_written != Some(value) {
                        match write(value) {
                            Ok(()) => last_written = Some(value),
                            Err(error) => {
                                eprintln!("[display] brightness {value} write failed: {error}")
                            }
                        }
                    }
                    if closing {
                        break;
                    }
                }
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        {
            let mut requests = self.shared.requests.lock().unwrap();
            requests.closing = true;
            self.shared.changed.notify_one();
        }
        if self
            .thread
            .take()
            .is_some_and(|thread| thread.join().is_err())
        {
            eprintln!("[display] backlight writer panicked during shutdown");
        }
    }
}

#[derive(Default)]
struct Contact {
    unsynchronized: bool,
    touching: bool,
    slot: i32,
    tracked: std::collections::HashSet<i32>,
    buttons: std::collections::HashSet<u16>,
}

impl Contact {
    fn is_touching(&self) -> bool {
        self.unsynchronized || self.touching || !self.tracked.is_empty() || !self.buttons.is_empty()
    }

    fn activity(&mut self, kind: u16, code: u16, value: i32) -> bool {
        if self.unsynchronized {
            return false;
        }
        match (kind, code) {
            (0, 3) => {
                // SYN_DROPPED: an empty event cache is not proof of release.
                self.unsynchronized = true;
                self.touching = false;
                self.tracked.clear();
                self.buttons.clear();
                false
            }
            (1, 0x14a) => {
                // BTN_TOUCH
                let was_touching = self.touching;
                self.touching = value > 0;
                if !self.touching {
                    self.tracked.clear();
                }
                was_touching || self.touching
            }
            (1, 0x110..=0x117) => {
                if value > 0 {
                    self.buttons.insert(code);
                } else {
                    self.buttons.remove(&code);
                }
                value > 0
            }
            (1, _) => value > 0 && code < 0x100,
            (2, _) => value != 0, // EV_REL: mouse motion or wheel.
            (3, 0x2f) => {
                self.slot = value;
                false
            } // ABS_MT_SLOT
            (3, 0x39) => {
                // ABS_MT_TRACKING_ID
                if value >= 0 {
                    self.tracked.insert(self.slot);
                    true
                } else {
                    self.tracked.remove(&self.slot)
                }
            }
            (3, _) => self.touching || !self.tracked.is_empty(),
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct PointerCapabilities {
    touch_button: bool,
    mouse_buttons: bool,
    multitouch: bool,
    slots: bool,
}

fn key_down(keys: &[u8], code: usize) -> bool {
    keys.get(code / 8)
        .is_some_and(|byte| byte & (1 << (code % 8)) != 0)
}

fn contact_snapshot(
    capabilities: PointerCapabilities,
    keys: &[u8],
    tracking: &[i32],
    slot: i32,
) -> Contact {
    let mut contact = Contact {
        slot,
        ..Contact::default()
    };
    if capabilities.touch_button {
        contact.touching = key_down(keys, 0x14a);
    }
    if capabilities.mouse_buttons {
        contact
            .buttons
            .extend((0x110..=0x117).filter(|code| key_down(keys, usize::from(*code))));
    }
    if capabilities.multitouch && capabilities.slots {
        contact.tracked.extend(
            tracking
                .iter()
                .enumerate()
                .filter(|(_, id)| **id >= 0)
                .map(|(slot, _)| slot as i32),
        );
    }
    contact
}

fn rotation_blocked(pointer_readers: usize, pointer_failed: bool, touching_readers: usize) -> bool {
    pointer_failed || pointer_readers == 0 || touching_readers > 0
}

fn dismisses_saver(kind: u16, code: u16, value: i32) -> bool {
    // Slint owns pointer dismissal. Its click can open Saver before queued
    // evdev down/move notifications run, so those notifications only rearm.
    kind == 1 && code < 0x100 && value > 0
}

#[cfg(all(feature = "frame", target_os = "linux"))]
pub use runtime::{start, Guard};

fn capability(value: &str, bit: usize, word_bits: usize) -> bool {
    value
        .split_whitespace()
        .rev()
        .nth(bit / word_bits)
        .and_then(|word| u64::from_str_radix(word, 16).ok())
        .is_some_and(|word| word & (1 << (bit % word_bits)) != 0)
}

#[cfg(all(feature = "frame", target_os = "linux"))]
mod runtime {
    use super::*;
    use chrono::Timelike;
    use slint::{ComponentHandle, Timer, TimerMode};
    use std::cell::{Cell, RefCell};
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::rc::{Rc, Weak};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    thread_local! {
        static CURRENT: RefCell<Weak<Controller>> = RefCell::new(Weak::new());
    }

    fn hardware_now() -> chrono::DateTime<chrono::Local> {
        chrono::Local::now()
    }

    struct Controller {
        config: Config,
        backlight: Backlight,
        writer: Arc<SharedWriter>,
        idle: Cell<bool>,
        input_available: Cell<bool>,
        last_brightness: Cell<Option<u8>>,
        night_active: Cell<bool>,
        night_callback: RefCell<Option<Box<dyn Fn(bool)>>>,
        activity_callback: RefCell<Option<Rc<dyn Fn(bool)>>>,
        idle_timer: Timer,
        night_timer: Timer,
    }

    impl Controller {
        fn apply(&self) -> bool {
            let minute = if self.config.night.is_some() {
                let now = hardware_now();
                (now.hour() * 60 + now.minute()) as u16
            } else {
                0
            };
            let night = self
                .config
                .night
                .is_some_and(|night| night.contains(minute));
            let night_changed = self.night_active.replace(night) != night;
            if night_changed {
                if let Some(callback) = self.night_callback.borrow().as_ref() {
                    callback(night);
                }
            }
            let percent = self.config.brightness(self.idle.get(), minute);
            if self.last_brightness.replace(Some(percent)) != Some(percent) {
                eprintln!(
                    "[display] {} brightness: {percent}%",
                    if self.idle.get() { "idle" } else { "active" }
                );
            }
            self.writer.request(self.backlight.raw(percent));
            night_changed
        }

        fn arm_idle(self: &Rc<Self>) {
            if let Some(delay) = self
                .config
                .idle_after
                .filter(|_| self.input_available.get())
            {
                let weak = Rc::downgrade(self);
                self.idle_timer
                    .start(TimerMode::SingleShot, delay, move || {
                        if let Some(controller) = weak.upgrade() {
                            controller.idle.set(true);
                            controller.apply();
                        }
                    });
            }
        }

        fn arm_night(self: &Rc<Self>) {
            if let Some(night) = self.config.night {
                let delay = next_night_boundary(hardware_now(), night);
                let weak = Rc::downgrade(self);
                self.night_timer
                    .start(TimerMode::SingleShot, delay, move || {
                        if let Some(controller) = weak.upgrade() {
                            controller.apply();
                            controller.arm_night();
                        }
                    });
            }
        }

        fn wake(self: &Rc<Self>) {
            self.idle.set(false);
            let night_changed = self.apply();
            self.arm_idle();
            if night_changed {
                self.arm_night();
            }
        }

        fn human_activity(self: &Rc<Self>, dismiss: bool) {
            self.wake();
            let callback = self.activity_callback.borrow().clone();
            if let Some(callback) = callback {
                callback(dismiss);
            }
        }

        fn inputs_lost(&self) {
            self.input_available.set(false);
            self.idle_timer.stop();
            self.idle.set(false);
            self.apply();
            eprintln!("[display] all activity inputs unavailable; idle dimming stopped");
        }
    }

    struct InputLifetime {
        alive: AtomicBool,
        pending: AtomicBool,
        pending_dismissal: AtomicBool,
        readers: AtomicUsize,
        touching_readers: AtomicUsize,
        pointer_readers: AtomicUsize,
        pointer_failed: AtomicBool,
    }

    struct BrightnessRestore(Writer);

    impl BrightnessRestore {
        fn spawn(
            restore: u32,
            write: impl FnMut(u32) -> io::Result<()> + Send + 'static,
        ) -> io::Result<Self> {
            Writer::spawn(restore, write).map(Self)
        }

        fn shared(&self) -> Arc<SharedWriter> {
            self.0.shared.clone()
        }
    }

    /// Retain until the event loop ends; normal drop restores the day brightness.
    pub struct Guard {
        controller: Rc<Controller>,
        inputs: Arc<InputLifetime>,
        _restore: BrightnessRestore,
    }

    impl Guard {
        pub fn is_touching(&self) -> bool {
            rotation_blocked(
                self.inputs.pointer_readers.load(Ordering::Acquire),
                self.inputs.pointer_failed.load(Ordering::Acquire),
                self.inputs.touching_readers.load(Ordering::Acquire),
            )
        }

        /// Use on the UI thread after an accepted physical orientation change.
        pub fn wake(&self) {
            self.controller.wake();
        }

        /// Emit the configured local-time night window now and when it changes.
        /// Activity restores day brightness without leaving the night window.
        pub fn on_night_changed(&self, callback: impl Fn(bool) + 'static) {
            callback(self.controller.night_active.get());
            *self.controller.night_callback.borrow_mut() = Some(Box::new(callback));
        }
        /// Pointer input rearms idle; keyboard presses also dismiss the saver.
        pub fn on_activity(&self, callback: impl Fn(bool) + 'static) {
            *self.controller.activity_callback.borrow_mut() = Some(Rc::new(callback));
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            CURRENT.with(|current| *current.borrow_mut() = Weak::new());
            self.inputs.alive.store(false, Ordering::Release);
            self.controller.idle_timer.stop();
            self.controller.night_timer.stop();
        }
    }

    type ActivityInput = (PathBuf, std::fs::File, Option<PointerCapabilities>);
    fn open_inputs() -> io::Result<(Vec<ActivityInput>, bool)> {
        let mut files = Vec::new();
        let mut pointer_failed = false;
        for entry in std::fs::read_dir("/sys/class/input")? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.strip_prefix("event").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
            }) {
                continue;
            }
            let capabilities = entry.path().join("device/capabilities");
            let events = std::fs::read_to_string(capabilities.join("ev"))?;
            let bits = usize::BITS as usize;
            let keys = std::fs::read_to_string(capabilities.join("key")).unwrap_or_default();
            let axes = std::fs::read_to_string(capabilities.join("abs")).unwrap_or_default();
            let pointer = PointerCapabilities {
                touch_button: capability(&keys, 0x14a, bits),
                mouse_buttons: (0x110..=0x117).any(|code| capability(&keys, code, bits)),
                multitouch: capability(&axes, 0x39, bits),
                slots: capability(&axes, 0x2f, bits),
            };
            let touch = pointer.touch_button || pointer.multitouch;
            if !capability(&events, 1, bits)
                && !capability(&events, 2, bits)
                && !(capability(&events, 3, bits) && touch)
            {
                continue;
            }
            let path = Path::new("/dev/input").join(name.as_ref());
            match std::fs::File::open(&path) {
                Ok(file) => files.push((
                    path,
                    file,
                    (touch || pointer.mouse_buttons).then_some(pointer),
                )),
                Err(error) => {
                    pointer_failed |= touch || pointer.mouse_buttons;
                    eprintln!("[display] cannot observe {}: {error}", path.display());
                }
            }
        }
        if files.is_empty() {
            return Err(io::Error::other("no readable activity input devices"));
        }
        Ok((files, pointer_failed))
    }

    fn read_ioctl(number: u8, bytes: usize) -> libc::c_ulong {
        ((2_u64 << 30) | ((bytes as u64) << 16) | (u64::from(b'E') << 8) | u64::from(number))
            as libc::c_ulong
    }

    fn resynchronize(
        file: &std::fs::File,
        capabilities: PointerCapabilities,
    ) -> io::Result<Contact> {
        let mut keys = [0_u8; 96]; // KEY_MAX is 0x2ff.
        if capabilities.touch_button || capabilities.mouse_buttons {
            // EVIOCGKEY reads current state without consuming or grabbing events.
            if unsafe {
                libc::ioctl(
                    file.as_raw_fd(),
                    read_ioctl(0x18, keys.len()),
                    keys.as_mut_ptr(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let mut slot = 0;
        let mut tracking = Vec::new();
        if capabilities.multitouch && capabilities.slots {
            let mut info = [0_i32; 6];
            // EVIOCGABS(ABS_MT_SLOT) bounds the EVIOCGMTSLOTS buffer.
            if unsafe {
                libc::ioctl(
                    file.as_raw_fd(),
                    read_ioctl(0x40 + 0x2f, std::mem::size_of_val(&info)),
                    info.as_mut_ptr(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            if info[1] != 0 || !(0..64).contains(&info[2]) || !(0..=info[2]).contains(&info[0]) {
                return Err(io::Error::other("invalid multitouch slot range"));
            }
            slot = info[0];
            let mut values = vec![0_i32; info[2] as usize + 2];
            values[0] = 0x39; // ABS_MT_TRACKING_ID
            if unsafe {
                libc::ioctl(
                    file.as_raw_fd(),
                    read_ioctl(0x0a, values.len() * std::mem::size_of::<i32>()),
                    values.as_mut_ptr(),
                )
            } < 0
            {
                return Err(io::Error::last_os_error());
            }
            tracking.extend_from_slice(&values[1..]);
        } else if capabilities.multitouch && !capabilities.touch_button {
            return Err(io::Error::other(
                "touch device has no queryable contact state",
            ));
        }
        Ok(contact_snapshot(capabilities, &keys, &tracking, slot))
    }

    fn replace_contact(contact: &mut Contact, snapshot: Contact, lifetime: &InputLifetime) {
        let before = contact.is_touching();
        *contact = snapshot;
        if before != contact.is_touching() {
            if before {
                lifetime.touching_readers.fetch_sub(1, Ordering::AcqRel);
            } else {
                lifetime.touching_readers.fetch_add(1, Ordering::AcqRel);
            }
        }
    }

    fn read_input(
        mut file: std::fs::File,
        path: PathBuf,
        pointer: Option<PointerCapabilities>,
        app: slint::Weak<crate::AppWindow>,
        lifetime: Arc<InputLifetime>,
    ) {
        let mut contact = Contact {
            unsynchronized: pointer.is_some(),
            ..Contact::default()
        };
        let mut resync_failed = false;
        if let Some(capabilities) = pointer {
            match resynchronize(&file, capabilities) {
                Ok(snapshot) => replace_contact(&mut contact, snapshot, &lifetime),
                Err(error) => {
                    eprintln!(
                        "[display] pointer state unknown for {}; rotation blocked: {error}",
                        path.display()
                    );
                    resync_failed = true;
                }
            }
        }
        let offset = 2 * std::mem::size_of::<isize>();
        let mut record = vec![0; offset + 8];
        // Each evdev open owns its event queue. Blocking reads neither poll nor
        // grab input from Slint; dormant readers end with the process.
        while lifetime.alive.load(Ordering::Acquire) {
            match file.read_exact(&mut record) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    eprintln!("[display] input {} stopped: {error}", path.display());
                    break;
                }
            }
            let kind = u16::from_ne_bytes(record[offset..offset + 2].try_into().unwrap());
            let code = u16::from_ne_bytes(record[offset + 2..offset + 4].try_into().unwrap());
            let value = i32::from_ne_bytes(record[offset + 4..offset + 8].try_into().unwrap());
            if contact.unsynchronized {
                if kind == 0 && code == 0 {
                    if let Some(capabilities) = pointer {
                        match resynchronize(&file, capabilities) {
                            Ok(snapshot) => {
                                replace_contact(&mut contact, snapshot, &lifetime);
                                if resync_failed {
                                    eprintln!(
                                        "[display] pointer state recovered for {}",
                                        path.display()
                                    );
                                    resync_failed = false;
                                }
                            }
                            Err(error) => {
                                if !resync_failed {
                                    eprintln!("[display] pointer resync failed for {}; rotation blocked: {error}", path.display());
                                    resync_failed = true;
                                }
                            }
                        }
                    } else {
                        contact = Contact::default();
                    }
                }
                continue;
            }
            let was_touching = contact.is_touching();
            let activity = contact.activity(kind, code, value);
            if pointer.is_some() && contact.is_touching() != was_touching {
                if was_touching {
                    lifetime.touching_readers.fetch_sub(1, Ordering::AcqRel);
                } else {
                    lifetime.touching_readers.fetch_add(1, Ordering::AcqRel);
                }
            }
            if activity && dismisses_saver(kind, code, value) {
                lifetime.pending_dismissal.store(true, Ordering::Release);
            }
            if activity
                && lifetime.alive.load(Ordering::Acquire)
                && !lifetime.pending.swap(true, Ordering::AcqRel)
            {
                let notification = lifetime.clone();
                if app
                    .upgrade_in_event_loop(move |_| {
                        notification.pending.store(false, Ordering::Release);
                        let dismiss = notification.pending_dismissal.swap(false, Ordering::AcqRel);
                        if notification.alive.load(Ordering::Acquire) {
                            CURRENT.with(|current| {
                                let controller = current.borrow().upgrade();
                                if let Some(controller) = controller {
                                    controller.human_activity(dismiss);
                                }
                            });
                        }
                    })
                    .is_err()
                {
                    lifetime.alive.store(false, Ordering::Release);
                }
            }
        }
        if pointer.is_some() {
            lifetime.pointer_failed.store(true, Ordering::Release);
            lifetime.pointer_readers.fetch_sub(1, Ordering::AcqRel);
            eprintln!(
                "[display] pointer observer {} stopped; rotation blocked",
                path.display()
            );
        }
        if pointer.is_some() && contact.is_touching() {
            lifetime.touching_readers.fetch_sub(1, Ordering::AcqRel);
        }
        if lifetime.readers.fetch_sub(1, Ordering::AcqRel) == 1
            && lifetime.alive.load(Ordering::Acquire)
        {
            let _ = app.upgrade_in_event_loop(move |_| {
                if lifetime.alive.load(Ordering::Acquire) {
                    CURRENT.with(|current| {
                        if let Some(controller) = current.borrow().upgrade() {
                            controller.inputs_lost();
                        }
                    });
                }
            });
        }
    }

    pub fn start(app: &crate::AppWindow) -> Option<Guard> {
        let configure = || -> Result<Guard, Box<dyn std::error::Error>> {
            if CURRENT.with(|current| current.borrow().upgrade().is_some()) {
                return Err("display controller already installed".into());
            }
            let config = Config::parse(|name| std::env::var(name).ok())?;
            let explicit = std::env::var_os("FRAME_BACKLIGHT_PATH").map(PathBuf::from);
            let backlight =
                Backlight::discover(Path::new("/sys/class/backlight"), explicit.as_deref())?;
            let (files, pointer_failed) = open_inputs()?;
            let day_brightness = backlight.raw(config.day);
            let hardware = backlight.clone();
            let restore = BrightnessRestore::spawn(day_brightness, move |value| hardware.write(value))?;
            let controller = Rc::new(Controller {
                config,
                backlight,
                writer: restore.shared(),
                idle: Cell::new(false),
                input_available: Cell::new(true),
                last_brightness: Cell::new(None),
                night_active: Cell::new(false),
                night_callback: RefCell::new(None),
                activity_callback: RefCell::new(None),
                idle_timer: Timer::default(),
                night_timer: Timer::default(),
            });
            let pointers = files
                .iter()
                .filter(|(_, _, pointer)| pointer.is_some())
                .count();
            let inputs = Arc::new(InputLifetime {
                alive: AtomicBool::new(true),
                pending: AtomicBool::new(false),
                pending_dismissal: AtomicBool::new(false),
                readers: AtomicUsize::new(files.len()),
                // Unknown until each pointer reader obtains a kernel snapshot.
                touching_readers: AtomicUsize::new(pointers),
                pointer_readers: AtomicUsize::new(pointers),
                pointer_failed: AtomicBool::new(pointer_failed),
            });
            CURRENT.with(|current| *current.borrow_mut() = Rc::downgrade(&controller));
            let guard = Guard {
                controller,
                inputs,
                _restore: restore,
            };
            let mut started = 0;
            for (path, file, pointer) in files {
                let (weak, lifetime) = (app.as_weak(), guard.inputs.clone());
                if let Err(error) = std::thread::Builder::new()
                    .name("frame-input".into())
                    .spawn(move || read_input(file, path, pointer, weak, lifetime))
                {
                    guard.inputs.readers.fetch_sub(1, Ordering::AcqRel);
                    if pointer.is_some() {
                        guard.inputs.pointer_failed.store(true, Ordering::Release);
                        guard.inputs.pointer_readers.fetch_sub(1, Ordering::AcqRel);
                        guard.inputs.touching_readers.fetch_sub(1, Ordering::AcqRel);
                    }
                    eprintln!("[display] input observer could not start: {error}");
                } else {
                    started += 1;
                }
            }
            if started == 0 || guard.inputs.readers.load(Ordering::Acquire) == 0 {
                return Err("no activity input readers available".into());
            }
            eprintln!("[display] observing {started} input devices; idle timeout: {:?}; night configured: {}", guard.controller.config.idle_after, guard.controller.config.night.is_some());
            guard.controller.wake();
            guard.controller.arm_night();
            Ok(guard)
        };
        match configure() {
            Ok(guard) => Some(guard),
            Err(error) => {
                eprintln!("[display] dimming unavailable: {error}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn touch_input_rearms_idle_without_dismissing_a_clicked_saver() {
        let mut contact = Contact::default();
        assert!(contact.activity(1, 0x14a, 1));
        assert!(!dismisses_saver(1, 0x14a, 1));
        assert!(contact.activity(3, 0x35, 300));
        assert!(!dismisses_saver(3, 0x35, 300));
        assert!(contact.activity(1, 0x14a, 0));
        assert!(!dismisses_saver(1, 0x14a, 0));
    }

    #[test]
    fn queued_coalesced_touch_after_manual_open_does_not_dismiss_saver() {
        let mut contact = Contact::default();
        let mut pending_dismissal = false;
        for (kind, code, value) in [(3, 0x39, 10), (3, 0x35, 300), (3, 0x39, -1)] {
            assert!(contact.activity(kind, code, value));
            pending_dismissal |= dismisses_saver(kind, code, value);
        }
        // The queued notification may run after Slint's clicked handler opens Saver.
        assert!(!pending_dismissal);
        assert!(!contact.is_touching());
    }

    #[test]
    fn keyboard_dismisses_saver_but_mouse_input_only_rearms_idle() {
        let mut contact = Contact::default();
        assert!(contact.activity(1, 30, 1));
        assert!(dismisses_saver(1, 30, 1));
        assert!(contact.activity(1, 30, 2));
        assert!(dismisses_saver(1, 30, 2));
        assert!(!contact.activity(1, 30, 0));
        assert!(!dismisses_saver(1, 30, 0));
        assert!(contact.activity(2, 0, -5));
        assert!(!dismisses_saver(2, 0, -5));
        assert!(contact.activity(1, 0x110, 1));
        assert!(!dismisses_saver(1, 0x110, 1));
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "photoframe-backlight-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn backlight(&self, name: &str, maximum: &str, brightness: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("max_brightness"), maximum).unwrap();
            std::fs::write(path.join("brightness"), brightness).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn config(values: &[(&str, &str)]) -> Result<Config, String> {
        Config::parse(|key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_string())
        })
    }

    #[test]
    fn default_keeps_screen_on_and_activity_restores_day() {
        let config = config(&[]).unwrap();
        assert_eq!(config.idle_after, Some(Duration::from_secs(180)));
        assert_eq!(config.night, None);
        assert_eq!(config.brightness(false, 0), 65);
        assert_eq!(config.brightness(true, 0), 20);
    }

    #[test]
    fn night_requires_both_times_and_wraps_midnight() {
        assert!(config(&[("FRAME_NIGHT_START", "22:00")]).is_err());
        let config =
            config(&[("FRAME_NIGHT_START", "22:00"), ("FRAME_NIGHT_END", "06:00")]).unwrap();
        for minute in [0, 359, 1320, 1439] {
            assert_eq!(config.brightness(true, minute), 10);
        }
        for minute in [360, 1319] {
            assert_eq!(config.brightness(true, minute), 20);
        }
        assert_eq!(config.brightness(false, 1380), 65);
    }

    #[test]
    fn next_boundary_is_future_and_uses_civil_night_start_and_end() {
        use chrono::TimeZone;
        let timezone = chrono::FixedOffset::east_opt(3600).unwrap();
        let night = Night {
            start: 22 * 60,
            end: 6 * 60,
            brightness: 10,
        };
        for (hour, minute, second, delay) in
            [(21, 59, 59, 1), (22, 0, 0, 8 * 3600), (6, 0, 0, 16 * 3600)]
        {
            let now = timezone
                .with_ymd_and_hms(2026, 10, 13, hour, minute, second)
                .unwrap();
            assert_eq!(next_night_boundary(now, night), Duration::from_secs(delay));
        }
    }

    #[test]
    fn rejects_blank_invalid_ranges_and_equal_night_times() {
        for values in [
            vec![("FRAME_DAY_BRIGHTNESS", "0")],
            vec![("FRAME_IDLE_BRIGHTNESS", "101")],
            vec![("FRAME_IDLE_DIM_SECONDS", "-1")],
            vec![("FRAME_IDLE_DIM_SECONDS", "86401")],
            vec![("FRAME_DAY_BRIGHTNESS", "10")],
            vec![("FRAME_NIGHT_START", "24:00"), ("FRAME_NIGHT_END", "06:00")],
            vec![("FRAME_NIGHT_START", "6:00"), ("FRAME_NIGHT_END", "06:00")],
            vec![("FRAME_NIGHT_START", "06:00"), ("FRAME_NIGHT_END", "06:00")],
        ] {
            assert!(config(&values).is_err(), "{values:?}");
        }
        assert!(config(&[("FRAME_IDLE_DIM_SECONDS", "0")])
            .unwrap()
            .idle_after
            .is_none());
    }

    #[test]
    fn adapter_discovers_one_device_and_maps_without_blanking() {
        let fixture = Fixture::new();
        let path = fixture.backlight("panel", "255\n", "166\n");
        let backlight = Backlight::discover(&fixture.0, None).unwrap();
        assert_eq!(backlight.raw(65), 166);
        assert_eq!(backlight.raw(20), 51);
        backlight.write(backlight.raw(20)).unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join("brightness")).unwrap(),
            "51\n"
        );
        assert!(backlight.write(0).is_err());
        assert!(backlight.write(256).is_err());
        fixture.backlight("second", "1", "1");
        assert!(Backlight::discover(&fixture.0, None).is_err());
        assert!(Backlight::discover(&fixture.0, Some(&path)).is_ok());
        assert_eq!(
            Backlight::from_directory(&fixture.0.join("second"))
                .unwrap()
                .raw(1),
            1
        );
    }

    #[test]
    fn brightness_write_is_one_numeric_sysfs_operation() {
        struct Sysfs {
            brightness: u32,
            writes: Vec<Vec<u8>>,
        }
        impl io::Write for Sysfs {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.writes.push(bytes.to_vec());
                let value = std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|text| text.trim().parse::<u32>().ok())
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
                self.brightness = value;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut sysfs = Sysfs {
            brightness: 166,
            writes: Vec::new(),
        };
        write_brightness(&mut sysfs, 51).unwrap();
        assert_eq!(sysfs.brightness, 51);
        assert_eq!(sysfs.writes, [b"51\n".to_vec()]);
    }

    #[test]
    fn adapter_rejects_malformed_and_out_of_range_values() {
        let fixture = Fixture::new();
        for (name, maximum, brightness) in
            [("zero", "0", "0"), ("bad", "x", "0"), ("large", "5", "6")]
        {
            assert!(
                Backlight::from_directory(&fixture.backlight(name, maximum, brightness)).is_err()
            );
        }
        assert!(Backlight::from_directory(Path::new("relative")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn adapter_rejects_attribute_symlinks_that_escape_the_selected_device() {
        let fixture = Fixture::new();
        let path = fixture.backlight("panel", "255", "166");
        let outside = fixture.0.join("outside");
        std::fs::write(&outside, "166").unwrap();
        std::fs::remove_file(path.join("brightness")).unwrap();
        std::os::unix::fs::symlink(&outside, path.join("brightness")).unwrap();
        assert!(Backlight::from_directory(&path).is_err());
    }

    fn wait_until_closing(shared: &SharedWriter) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !shared.requests.lock().unwrap().closing {
            assert!(
                std::time::Instant::now() < deadline,
                "shutdown did not reach writer"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn shutdown_restores_day_after_an_in_flight_dim_and_drops_stale_requests() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let values = Arc::new(Mutex::new(Vec::new()));
        let written = values.clone();
        let writer = Writer::spawn(65, move |value| {
            if value == 20 {
                started_tx.send(()).unwrap();
                continue_rx.recv().unwrap();
            }
            written.lock().unwrap().push(value);
            Ok(())
        })
        .unwrap();
        writer.shared.request(20);
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        writer.shared.request(10);
        let shared = writer.shared.clone();
        let shutdown = std::thread::spawn(move || drop(writer));
        wait_until_closing(&shared);
        shared.request(5);
        continue_tx.send(()).unwrap();
        shutdown.join().unwrap();
        assert_eq!(*values.lock().unwrap(), [20, 65]);
    }

    #[test]
    fn raw_input_ignores_sensors_and_proximity_but_accepts_human_activity() {
        let mut contact = Contact::default();
        assert!(!contact.activity(3, 0, 120));
        assert!(!contact.activity(1, 0x145, 1));
        assert!(!contact.activity(0, 0, 0));
        assert!(contact.activity(1, 30, 1));
        assert!(contact.activity(1, 30, 2));
        assert!(!contact.activity(1, 30, 0));
        assert!(contact.activity(2, 0, -3));
        assert!(!contact.activity(2, 0, 0));
        assert!(contact.activity(1, 0x110, 1));
        assert!(contact.activity(1, 0x14a, 1));
        assert!(contact.activity(3, 1, 300));
        assert!(contact.activity(1, 0x14a, 0));
        assert!(!contact.activity(3, 1, 300));
    }

    #[test]
    fn multitouch_tracks_each_contact_and_recovers_after_dropped_events() {
        let mut contact = Contact::default();
        assert!(!contact.activity(3, 0x2f, 0));
        assert!(contact.activity(3, 0x39, 100));
        assert!(contact.is_touching());
        assert!(contact.activity(3, 0x35, 200));
        assert!(!contact.activity(3, 0x2f, 1));
        assert!(contact.activity(3, 0x39, 101));
        assert!(contact.activity(3, 0x39, -1));
        assert!(contact.activity(3, 0x36, 400));
        assert!(!contact.activity(0, 3, 0));
        assert!(contact.is_touching());
        assert!(!contact.activity(3, 0x35, 200));
        contact = contact_snapshot(
            PointerCapabilities {
                multitouch: true,
                slots: true,
                ..PointerCapabilities::default()
            },
            &[],
            &[-1, -1],
            0,
        );
        assert!(!contact.is_touching());
    }

    #[test]
    fn pointer_contact_survives_one_of_multiple_button_releases() {
        let mut contact = Contact::default();
        contact.activity(1, 0x110, 1);
        contact.activity(1, 0x111, 1);
        assert!(contact.is_touching());
        contact.activity(1, 0x110, 0);
        assert!(contact.is_touching());
        contact.activity(1, 0x111, 0);
        assert!(!contact.is_touching());
    }

    #[test]
    fn kernel_snapshot_includes_contacts_that_predate_the_reader() {
        let mut keys = [0_u8; 96];
        keys[0x14a / 8] |= 1 << (0x14a % 8);
        let touch = PointerCapabilities {
            touch_button: true,
            multitouch: true,
            slots: true,
            ..PointerCapabilities::default()
        };
        assert!(contact_snapshot(touch, &keys, &[-1, 42, -1], 1).is_touching());
        keys.fill(0);
        assert!(contact_snapshot(touch, &keys, &[-1, 42, -1], 1).is_touching());
        assert!(!contact_snapshot(touch, &keys, &[-1, -1, -1], 1).is_touching());
        keys[0x110 / 8] |= 1 << (0x110 % 8);
        assert!(contact_snapshot(
            PointerCapabilities {
                mouse_buttons: true,
                ..PointerCapabilities::default()
            },
            &keys,
            &[],
            0
        )
        .is_touching());
    }

    #[test]
    fn rotation_requires_a_known_live_pointer_observer_and_release() {
        assert!(rotation_blocked(0, false, 0));
        assert!(rotation_blocked(1, true, 0));
        assert!(rotation_blocked(1, false, 1));
        assert!(!rotation_blocked(1, false, 0));
        let mut contact = Contact::default();
        contact.activity(0, 3, 0);
        assert!(contact.is_touching());
        assert!(!contact.activity(1, 0x14a, 0));
        assert!(contact.is_touching());
        let caps = PointerCapabilities {
            touch_button: true,
            ..PointerCapabilities::default()
        };
        contact = contact_snapshot(caps, &[0; 96], &[], 0);
        assert!(!contact.is_touching());
    }

    #[test]
    fn kernel_capabilities_decode_32_and_64_bit_word_layouts() {
        assert!(capability("2000000 0", 0x39, 32));
        assert!(capability("200000000000000", 0x39, 64));
        assert!(capability("400 0 0 0 0 0 0 0 0 0 0", 0x14a, 32));
        assert!(capability("400 0 0 0 0 0", 0x14a, 64));
        assert!(!capability("8", 1, 32));
        assert!(capability("8", 3, 64));
        assert!(!capability("garbage", 0, 64));
    }
}
