//! Calibrated gravity poses select UI compensation; uncertain samples hold the last pose.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const GRAVITY: f64 = 9.80665;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    PositiveX,
    NegativeX,
    PositiveY,
    NegativeY,
}

impl Axis {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "+x" => Some(Self::PositiveX),
            "-x" => Some(Self::NegativeX),
            "+y" => Some(Self::PositiveY),
            "-y" => Some(Self::NegativeY),
            _ => None,
        }
    }
    fn index(self) -> usize {
        match self {
            Self::PositiveX | Self::NegativeX => 0,
            _ => 1,
        }
    }
    fn project(self, sample: [f64; 3]) -> f64 {
        sample[self.index()]
            * match self {
                Self::NegativeX | Self::NegativeY => -1.0,
                _ => 1.0,
            }
    }
}

#[derive(Clone, Copy, Debug)]
struct Calibration {
    upright: Axis,
    clockwise: Axis,
}

impl Calibration {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let axis = |name| {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .and_then(Axis::parse)
                .ok_or_else(|| format!("{name} must be +x, -x, +y, or -y"))
        };
        let upright = axis("upright_axis")?;
        let clockwise = axis("clockwise_axis")?;
        if upright.index() == clockwise.index() {
            return Err("calibration axes must be orthogonal".into());
        }
        Ok(Self { upright, clockwise })
    }

    fn angle(self, sample: [f64; 3]) -> Option<i32> {
        let norm = sample.iter().map(|value| value * value).sum::<f64>();
        if !norm.is_finite() || !(0.85_f64.powi(2)..=1.15_f64.powi(2)).contains(&norm) {
            return None;
        }
        let planar = sample[0].hypot(sample[1]);
        if planar < 0.50 {
            return None;
        }
        let upright = self.upright.project(sample) / planar;
        let clockwise = self.clockwise.project(sample) / planar;
        // Keep a dead band around diagonal poses even when the stand is tilted.
        if upright.abs().max(clockwise.abs()) < 0.82 {
            return None;
        }
        if upright.abs() > clockwise.abs() {
            Some(if upright > 0.0 { 0 } else { 180 })
        } else {
            Some(if clockwise > 0.0 { 270 } else { 90 })
        }
    }
}

struct Candidate {
    angle: i32,
    count: u8,
    sample: [f64; 3],
}
struct Policy {
    calibration: Calibration,
    angle: i32,
    candidate: Option<Candidate>,
}

impl Policy {
    fn new(calibration: Calibration) -> Self {
        Self {
            calibration,
            angle: 0,
            candidate: None,
        }
    }

    fn update(&mut self, sample: [f64; 3]) -> Option<i32> {
        let Some(angle) = self.calibration.angle(sample) else {
            self.candidate = None;
            return None;
        };
        if angle == self.angle {
            self.candidate = None;
            return None;
        }
        let count = self
            .candidate
            .as_ref()
            .filter(|candidate| {
                candidate.angle == angle
                    && candidate
                        .sample
                        .iter()
                        .zip(sample)
                        .map(|(before, after)| (before - after).powi(2))
                        .sum::<f64>()
                        <= 0.15_f64.powi(2)
            })
            .map_or(1, |candidate| candidate.count + 1);
        if count >= 3 {
            self.angle = angle;
            self.candidate = None;
            Some(angle)
        } else {
            self.candidate = Some(Candidate {
                angle,
                count,
                sample,
            });
            None
        }
    }

    fn target(&mut self, sample: Option<[f64; 3]>, applied: i32) -> i32 {
        let classified = sample.and_then(|sample| self.calibration.angle(sample));
        let Some(classified) = classified else {
            self.angle = applied;
            self.candidate = None;
            return applied;
        };
        if self.angle != applied && self.angle != classified {
            self.angle = applied;
            self.candidate = None;
        }
        self.update(sample.unwrap());
        if self.angle == classified {
            self.angle
        } else {
            applied
        }
    }
}

fn compatible_backend_rotation(value: Option<&str>) -> bool {
    value.is_none_or(|value| value.parse::<i32>().ok() == Some(0))
}

struct Sensor {
    directory: PathBuf,
    scales: [f64; 3],
}

impl Sensor {
    fn discover(class: &Path) -> io::Result<Self> {
        let mut devices = Vec::new();
        for entry in std::fs::read_dir(class)? {
            let entry = entry?;
            if std::fs::read_to_string(entry.path().join("name"))
                .ok()
                .is_some_and(|name| name.trim() == "sc7a20")
            {
                devices.push(entry.path());
            }
        }
        if devices.len() != 1 {
            return Err(io::Error::other("expected exactly one sc7a20 IIO device"));
        }
        Self::prepare(&devices[0])
    }

    fn prepare(directory: &Path) -> io::Result<Self> {
        let available = std::fs::read_to_string(directory.join("sampling_frequency_available"))?;
        if !available
            .split_whitespace()
            .any(|value| value.parse::<f64>().ok() == Some(50.0))
        {
            return Err(io::Error::other("sc7a20 does not advertise 50 Hz sampling"));
        }
        let mut scales = [0.0; 3];
        for (index, axis) in ["x", "y", "z"].iter().enumerate() {
            if !directory.join(format!("in_accel_{axis}_raw")).is_file() {
                return Err(io::Error::other("missing raw accelerometer axis"));
            }
            scales[index] =
                std::fs::read_to_string(directory.join(format!("in_accel_{axis}_scale")))?
                    .trim()
                    .parse::<f64>()
                    .map_err(io::Error::other)?;
            if !scales[index].is_finite() || scales[index] <= 0.0 || scales[index] > 1.0 {
                return Err(io::Error::other("invalid accelerometer scale"));
            }
        }
        use std::io::Write;
        let mut frequency = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(directory.join("sampling_frequency"))?;
        frequency.write_all(b"50\n")?;
        if std::fs::read_to_string(directory.join("sampling_frequency"))?
            .trim()
            .parse::<f64>()
            .ok()
            != Some(50.0)
        {
            return Err(io::Error::other("sc7a20 50 Hz sampling did not apply"));
        }
        Ok(Self {
            directory: directory.to_owned(),
            scales,
        })
    }

    fn read(&self, running: &AtomicBool) -> io::Result<Option<[f64; 3]>> {
        let mut sample = [0.0; 3];
        for (index, axis) in ["x", "y", "z"].iter().enumerate() {
            // Raw reads wait two sensor periods; don't start another axis after stop.
            if !running.load(Ordering::Acquire) {
                return Ok(None);
            }
            let raw = std::fs::read_to_string(self.directory.join(format!("in_accel_{axis}_raw")))?
                .trim()
                .parse::<i32>()
                .map_err(io::Error::other)?;
            sample[index] = f64::from(raw) * self.scales[index] / GRAVITY;
        }
        Ok(Some(sample))
    }
}

#[cfg(all(feature = "frame", target_os = "linux"))]
pub use runtime::{start, Guard};

#[cfg(all(feature = "frame", target_os = "linux"))]
mod runtime {
    use super::*;
    use slint::ComponentHandle;
    use std::cell::RefCell;
    use std::rc::{Rc, Weak};
    use std::sync::{atomic::AtomicI32, Arc, Condvar, Mutex};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    type Apply = dyn Fn(&crate::AppWindow, i32) -> bool;
    struct Handler {
        apply: Box<Apply>,
    }
    thread_local! { static HANDLER: RefCell<Weak<Handler>> = RefCell::new(Weak::new()); }
    struct Mailbox {
        desired: i32,
        queued: bool,
    }
    struct Shared {
        running: AtomicBool,
        applied: AtomicI32,
        mailbox: Mutex<Mailbox>,
        finished: Mutex<bool>,
        changed: Condvar,
    }

    impl Shared {
        fn post(self: &Arc<Self>, app: &slint::Weak<crate::AppWindow>, angle: i32) {
            let mut mailbox = self.mailbox.lock().unwrap();
            mailbox.desired = angle;
            if mailbox.queued
                || self.applied.load(Ordering::Acquire) == angle
                || !self.running.load(Ordering::Acquire)
            {
                return;
            }
            mailbox.queued = true;
            drop(mailbox);
            let shared = self.clone();
            if app
                .upgrade_in_event_loop(move |app| {
                    let angle = {
                        let mut mailbox = shared.mailbox.lock().unwrap();
                        mailbox.queued = false;
                        mailbox.desired
                    };
                    if shared.running.load(Ordering::Acquire)
                        && shared.applied.load(Ordering::Acquire) != angle
                    {
                        let accepted = HANDLER.with(|slot| {
                            slot.borrow()
                                .upgrade()
                                .is_some_and(|handler| (handler.apply)(&app, angle))
                        });
                        if accepted {
                            shared.applied.store(angle, Ordering::Release);
                        }
                    }
                })
                .is_err()
            {
                self.running.store(false, Ordering::Release);
            }
        }
    }

    /// Retain until shutdown; a rejected UI callback stays pending until accepted.
    pub struct Guard {
        shared: Arc<Shared>,
        _handler: Rc<Handler>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            HANDLER.with(|slot| *slot.borrow_mut() = Weak::new());
            let finished = self.shared.finished.lock().unwrap();
            self.shared.running.store(false, Ordering::Release);
            self.shared.changed.notify_all();
            let (finished, _) = self
                .shared
                .changed
                .wait_timeout_while(finished, Duration::from_millis(250), |finished| !*finished)
                .unwrap();
            if *finished {
                if self
                    .thread
                    .take()
                    .is_some_and(|thread| thread.join().is_err())
                {
                    eprintln!("[orientation] sensor worker failed during shutdown");
                }
            } else {
                // Kernel I/O cannot be cancelled; inactive callbacks are already gated.
                eprintln!("[orientation] sensor read exceeded shutdown deadline; worker detached");
            }
        }
    }

    fn run(app: slint::Weak<crate::AppWindow>, shared: Arc<Shared>, calibration: Calibration) {
        let work = || -> io::Result<()> {
            if !shared.running.load(Ordering::Acquire) {
                return Ok(());
            }
            let sensor = Sensor::discover(Path::new("/sys/bus/iio/devices"))?;
            eprintln!("[orientation] calibrated sc7a20 at 50 Hz; sampling poses at 2 Hz");
            let mut policy = Policy::new(calibration);
            let mut failed = false;
            while shared.running.load(Ordering::Acquire) {
                let started = Instant::now();
                let sample = match sensor.read(&shared.running) {
                    Ok(Some(sample)) => {
                        if failed {
                            eprintln!("[orientation] accelerometer readings recovered");
                            failed = false;
                        }
                        Some(sample)
                    }
                    Ok(None) => break,
                    Err(error) => {
                        if !failed {
                            eprintln!("[orientation] invalid sensor read; holding last orientation: {error}");
                            failed = true;
                        }
                        None
                    }
                };
                let desired = policy.target(sample, shared.applied.load(Ordering::Acquire));
                shared.post(&app, desired);
                let finished = shared.finished.lock().unwrap();
                let _ = shared
                    .changed
                    .wait_timeout_while(
                        finished,
                        Duration::from_millis(500).saturating_sub(started.elapsed()),
                        |_| shared.running.load(Ordering::Acquire),
                    )
                    .unwrap();
            }
            Ok(())
        };
        if let Err(error) = work() {
            eprintln!("[orientation] unavailable; fixed orientation: {error}");
        }
        *shared.finished.lock().unwrap() = true;
        shared.changed.notify_all();
    }

    pub fn start(
        app: &crate::AppWindow,
        apply: impl Fn(&crate::AppWindow, i32) -> bool + 'static,
    ) -> Option<Guard> {
        if std::env::var("FRAME_AUTO_ROTATE").is_ok_and(|value| value == "0") {
            return None;
        }
        if !compatible_backend_rotation(std::env::var("SLINT_KMS_ROTATION").ok().as_deref()) {
            eprintln!(
                "[orientation] calibrated rotation disabled: SLINT_KMS_ROTATION must be unset or 0"
            );
            return None;
        }
        let configure = || -> Result<Guard, Box<dyn std::error::Error>> {
            if HANDLER.with(|slot| slot.borrow().upgrade().is_some()) {
                return Err("orientation controller already installed".into());
            }
            let path = std::env::var_os("FRAME_ORIENTATION_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lib/photoframe-proto/orientation.json"));
            if std::fs::metadata(&path)?.len() > 4096 {
                return Err("orientation calibration file too large".into());
            }
            let calibration = Calibration::parse(&std::fs::read(path)?)?;
            let handler = Rc::new(Handler {
                apply: Box::new(apply),
            });
            let shared = Arc::new(Shared {
                running: AtomicBool::new(true),
                applied: AtomicI32::new(0),
                mailbox: Mutex::new(Mailbox {
                    desired: 0,
                    queued: false,
                }),
                finished: Mutex::new(false),
                changed: Condvar::new(),
            });
            let (weak, worker) = (app.as_weak(), shared.clone());
            let thread = std::thread::Builder::new()
                .name("frame-orientation".into())
                .spawn(move || run(weak, worker, calibration))?;
            HANDLER.with(|slot| *slot.borrow_mut() = Rc::downgrade(&handler));
            Ok(Guard {
                shared,
                _handler: handler,
                thread: Some(thread),
            })
        };
        match configure() {
            Ok(guard) => Some(guard),
            Err(error) => {
                eprintln!("[orientation] no calibrated rotation; fixed orientation: {error}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn calibration(clockwise: &str) -> Calibration {
        Calibration::parse(
            format!(r#"{{"upright_axis":"-y","clockwise_axis":"{clockwise}"}}"#).as_bytes(),
        )
        .unwrap()
    }
    fn settle(policy: &mut Policy, sample: [f64; 3]) -> Option<i32> {
        assert_eq!(policy.update(sample), None);
        assert_eq!(policy.update(sample), None);
        policy.update(sample)
    }

    #[test]
    fn four_verified_poses_compensate_physical_rotation() {
        let mut policy = Policy::new(calibration("+x"));
        assert_eq!(settle(&mut policy, [0.036, -1.006, 0.063]), None);
        assert_eq!(settle(&mut policy, [1.026, -0.007, 0.135]), Some(270));
        assert_eq!(settle(&mut policy, [0.004, 1.033, -0.030]), Some(180));
        assert_eq!(settle(&mut policy, [-1.017, -0.003, 0.055]), Some(90));
        assert_eq!(settle(&mut policy, [0.036, -1.006, 0.063]), Some(0));
    }

    #[test]
    fn opposite_mount_sign_is_supported_without_guessing() {
        let calibration = calibration("-x");
        assert_eq!(calibration.angle([-1.0, 0.0, 0.0]), Some(270));
        assert_eq!(calibration.angle([1.0, 0.0, 0.0]), Some(90));
        assert_eq!(calibration.angle([0.0, -1.0, 0.0]), Some(0));
        assert_eq!(calibration.angle([0.0, 1.0, 0.0]), Some(180));
    }

    #[test]
    fn tilted_stands_map_all_four_poses_with_enough_planar_gravity() {
        let calibration = calibration("+x");
        for (sample, angle) in [
            ([0.0, -0.6, 0.8], 0),
            ([0.6, 0.0, 0.8], 270),
            ([0.0, 0.6, 0.8], 180),
            ([-0.6, 0.0, 0.8], 90),
            ([0.680955, -0.091994, 0.825945], 270),
        ] {
            assert_eq!(calibration.angle(sample), Some(angle));
        }
        assert_eq!(calibration.angle([0.5, 0.0, 0.75_f64.sqrt()]), Some(270));
        assert_eq!(calibration.angle([0.49, 0.0, (1.0 - 0.49_f64.powi(2)).sqrt()]), None);
    }

    #[test]
    fn tilted_direction_retains_the_diagonal_dead_band() {
        let calibration = calibration("+x");
        let sample = |degrees: f64| {
            let radians = degrees.to_radians();
            [0.6 * radians.cos(), 0.6 * radians.sin(), 0.8]
        };
        assert_eq!(calibration.angle(sample(34.0)), Some(270));
        assert_eq!(calibration.angle(sample(35.0)), None);
        assert_eq!(calibration.angle(sample(45.0)), None);
    }

    #[test]
    fn configuration_rejects_missing_parallel_or_unsigned_axes() {
        for bytes in [
            b"{}".as_slice(),
            br#"{"upright_axis":"-y","clockwise_axis":"+y"}"#,
            br#"{"upright_axis":"z","clockwise_axis":"+x"}"#,
            br#"{"upright_axis":"-y","clockwise_axis":"x"}"#,
            b"invalid",
        ] {
            assert!(Calibration::parse(bytes).is_err());
        }
        assert!(Calibration::parse(br#"{"upright_axis":"+x","clockwise_axis":"-y"}"#).is_ok());
    }

    #[test]
    fn flat_facedown_and_boundary_samples_hold_the_last_rotation() {
        let mut policy = Policy::new(calibration("+x"));
        assert_eq!(settle(&mut policy, [1.0, 0.0, 0.0]), Some(270));
        for sample in [
            [0.0, 0.0, 1.0],
            [0.0, 0.0, -1.0],
            [0.707, 0.707, 0.0],
            [0.79, 0.60, 0.0],
        ] {
            for _ in 0..4 {
                assert_eq!(policy.update(sample), None);
            }
            assert_eq!(policy.angle, 270);
        }
    }

    #[test]
    fn movement_and_inconsistent_samples_cannot_complete_a_pose() {
        let mut policy = Policy::new(calibration("+x"));
        for _ in 0..4 {
            assert_eq!(policy.update([0.86, 0.10, 0.20]), None);
            assert_eq!(policy.update([1.08, -0.12, 0.20]), None);
        }
        for sample in [[2.0, 0.0, 0.0], [0.2, 0.0, 0.0], [f64::NAN, 0.0, 0.0]] {
            assert_eq!(policy.update(sample), None);
        }
        assert_eq!(policy.angle, 0);
        assert_eq!(settle(&mut policy, [1.0, 0.0, 0.0]), Some(270));
    }

    #[test]
    fn interrupted_candidate_requires_three_new_stable_samples() {
        let mut policy = Policy::new(calibration("+x"));
        assert_eq!(policy.update([-1.0, 0.0, 0.0]), None);
        assert_eq!(policy.update([-1.0, 0.0, 0.0]), None);
        assert_eq!(policy.update([0.0, 0.0, 1.0]), None);
        assert_eq!(settle(&mut policy, [-1.0, 0.0, 0.0]), Some(90));
        assert_eq!(policy.update([-1.0, 0.0, 0.0]), None);
    }

    #[test]
    fn deferred_rotation_retries_only_while_pose_confidence_remains_valid() {
        let mut policy = Policy::new(calibration("+x"));
        let clockwise = Some([1.0, 0.0, 0.0]);
        for _ in 0..2 {
            assert_eq!(policy.target(clockwise, 0), 0);
        }
        assert_eq!(policy.target(clockwise, 0), 270);
        assert_eq!(policy.target(clockwise, 0), 270);
        assert_eq!(policy.target(Some([0.0, 0.0, 1.0]), 0), 0);
        for _ in 0..2 {
            assert_eq!(policy.target(clockwise, 0), 0);
        }
        assert_eq!(policy.target(clockwise, 0), 270);
        assert_eq!(policy.target(Some([0.0, 1.0, 0.0]), 0), 0);
        assert_eq!(policy.target(None, 0), 0);
        for _ in 0..2 {
            assert_eq!(policy.target(clockwise, 0), 0);
        }
        assert_eq!(policy.target(clockwise, 0), 270);
        assert_eq!(policy.target(clockwise, 270), 270);
        assert_eq!(policy.target(None, 270), 270);
    }

    #[test]
    fn calibrated_ui_rotation_rejects_a_second_backend_transform() {
        assert!(compatible_backend_rotation(None));
        assert!(compatible_backend_rotation(Some("0")));
        for value in ["90", "180", "270", "360", "invalid", ""] {
            assert!(!compatible_backend_rotation(Some(value)));
        }
    }

    #[test]
    fn sensor_discovery_uses_exact_name_scales_and_complete_frequency_write() {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "pf-orientation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let thermal = root.join("thermal");
        std::fs::create_dir(&thermal).unwrap();
        std::fs::write(thermal.join("name"), "sun4i-gpadc-iio\n").unwrap();
        assert!(Sensor::discover(&root).is_err());
        let sensor = root.join("accelerometer");
        std::fs::create_dir(&sensor).unwrap();
        for (name, value) in [
            ("name", "sc7a20\n"),
            ("sampling_frequency_available", "1 10 25 50 100\n"),
            ("sampling_frequency", "1\n"),
            ("in_accel_x_raw", "36\n"),
            ("in_accel_y_raw", "-1006\n"),
            ("in_accel_z_raw", "63\n"),
            ("in_accel_x_scale", "0.009806\n"),
            ("in_accel_y_scale", "0.009806\n"),
            ("in_accel_z_scale", "0.009806\n"),
        ] {
            std::fs::write(sensor.join(name), value).unwrap();
        }
        let adapter = Sensor::discover(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(sensor.join("sampling_frequency")).unwrap(),
            "50\n"
        );
        let running = AtomicBool::new(true);
        let sample = adapter.read(&running).unwrap().unwrap();
        assert!((sample[1] + 1.006).abs() < 0.001);
        assert_eq!(calibration("+x").angle(sample), Some(0));
        running.store(false, Ordering::Release);
        assert_eq!(adapter.read(&running).unwrap(), None);
        std::fs::write(sensor.join("sampling_frequency_available"), "1 10\n").unwrap();
        assert!(Sensor::discover(&root).is_err());
        std::fs::write(sensor.join("sampling_frequency_available"), "50\n").unwrap();
        std::fs::write(sensor.join("in_accel_x_scale"), "NaN\n").unwrap();
        assert!(Sensor::discover(&root).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
