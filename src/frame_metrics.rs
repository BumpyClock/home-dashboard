use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use slint::{ComponentHandle, GraphicsAPI, Model, RenderingState, Timer, TimerMode};

use crate::{AppWindow, Frame};

const MAX_FRAMES: usize = 18_000;
const WARMUP_SECONDS: u64 = 30;

struct Sample {
    start_us: u64,
    start_interval_us: Option<u64>,
    renderer_callback_wall_us: u64,
    phase: &'static str,
    tab: i32,
    photo: i32,
}

struct Capture {
    origin: Instant,
    frames: Vec<Sample>,
    pending: Option<Sample>,
    previous_start_us: Option<u64>,
    phase: &'static str,
    actions: Vec<Value>,
    dropped_frames: usize,
    gl_query_attempted: bool,
    gl_limits: Option<(i32, i32)>,
    armed: bool,
    done: bool,
}

pub struct MetricsGuard {
    _warmup: Timer,
    _finish: Rc<Timer>,
}

impl Capture {
    fn elapsed_us(&self) -> u64 {
        self.origin.elapsed().as_micros() as u64
    }
}

#[derive(Clone, Copy)]
enum Action {
    Tab(i32),
    NextPhoto,
    Observe,
}

const WORKLOAD: &[(u64, &str, Action)] = &[
    (0, "home_startup", Action::Tab(0)),
    (5, "home_photo_1", Action::NextPhoto),
    (8, "home_photo_2", Action::NextPhoto),
    (11, "settings_entry", Action::Tab(5)),
    (13, "settings_idle", Action::Observe),
    (18, "home_return_1", Action::Tab(0)),
    (22, "settings_return_1", Action::Tab(5)),
    (26, "home_return_2", Action::Tab(0)),
    (30, "settings_return_2", Action::Tab(5)),
    (34, "home_return_3", Action::Tab(0)),
    (38, "home_photo_3", Action::NextPhoto),
    (42, "settings_return_3", Action::Tab(5)),
    (46, "home_return_4", Action::Tab(0)),
    (50, "home_photo_4", Action::NextPhoto),
    (54, "settings_return_4", Action::Tab(5)),
    (56, "settings_final_idle", Action::Observe),
];

fn cpu_ticks(stat: &str) -> Option<(u64, u64)> {
    // The process name in parentheses can contain spaces and parentheses.
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((fields.get(11)?.parse().ok()?, fields.get(12)?.parse().ok()?))
}

fn process_sample() -> Value {
    let ticks = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| cpu_ticks(&s));
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kib = |name: &str| -> Option<u64> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    json!({
        "user_ticks": ticks.map(|t| t.0),
        "system_ticks": ticks.map(|t| t.1),
        "rss_kib": kib("VmRSS:"),
        "peak_rss_kib": kib("VmHWM:"),
    })
}

const GL_MAX_TEXTURE_SIZE: u32 = 0x0D33;
const GL_MAX_RENDERBUFFER_SIZE: u32 = 0x84E8;

fn query_gl_limits(api: &GraphicsAPI<'_>) -> Option<(i32, i32)> {
    let GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
        return None;
    };
    let address = get_proc_address(c"glGetIntegerv");
    if address.is_null() {
        return None;
    }
    let (mut texture, mut renderbuffer) = (0, 0);
    // Slint keeps the GL context current during these rendering notifications.
    // Both GLES 2 limits write one GLint, and the query changes no GL state.
    unsafe {
        let get_integer: unsafe extern "system" fn(u32, *mut i32) = std::mem::transmute(address);
        get_integer(GL_MAX_TEXTURE_SIZE, &mut texture);
        get_integer(GL_MAX_RENDERBUFFER_SIZE, &mut renderbuffer);
    }
    (texture > 0 && renderbuffer > 0).then_some((texture, renderbuffer))
}

fn perform(
    app: &AppWindow,
    capture: &Rc<RefCell<Capture>>,
    scheduled_s: u64,
    phase: &'static str,
    action: Action,
) {
    if capture.borrow().done {
        return;
    }
    let f = app.global::<Frame>();
    let before_tab = f.get_active_tab();
    let before_photo = f.get_photo_index();
    let photo_count = f.get_photos().row_count();
    match action {
        Action::Tab(tab) => f.set_active_tab(tab),
        Action::NextPhoto if photo_count > 1 => {
            f.set_photo_index((before_photo + 1).rem_euclid(photo_count as i32));
        }
        _ => {}
    }
    let mut capture = capture.borrow_mut();
    capture.phase = phase;
    let at_us = capture.elapsed_us();
    capture.actions.push(json!({
        "scheduled_s": scheduled_s, "at_us": at_us, "phase": phase,
        "before_tab": before_tab, "before_photo": before_photo,
        "tab": f.get_active_tab(), "photo": f.get_photo_index(), "photo_count": photo_count,
    }));
}

/// Retain the returned timers until the application's event loop ends.
pub fn install(app: &AppWindow) -> Option<MetricsGuard> {
    let output = PathBuf::from(std::env::var_os("FRAME_METRICS_OUTPUT")?);
    if !output.is_absolute() {
        eprintln!("[metrics] FRAME_METRICS_OUTPUT must be an absolute external path");
        return None;
    }
    let parent = match output.parent().and_then(|p| p.canonicalize().ok()) {
        Some(parent) => parent,
        None => {
            eprintln!("[metrics] output directory must already exist");
            return None;
        }
    };
    if [
        std::env::current_dir().ok(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .ok(),
    ]
    .into_iter()
    .flatten()
    .any(|source| parent.starts_with(source))
    {
        eprintln!("[metrics] output must be outside the application/source directory");
        return None;
    }
    let benchmark = std::env::var("FRAME_BENCHMARK").is_ok_and(|v| v == "1");
    let seconds = std::env::var("FRAME_METRICS_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(5, 180);
    let seconds = if benchmark { seconds.max(60) } else { seconds };
    let capture = Rc::new(RefCell::new(Capture {
        origin: Instant::now(),
        frames: Vec::with_capacity(MAX_FRAMES),
        pending: None,
        previous_start_us: None,
        phase: "manual",
        actions: Vec::with_capacity(WORKLOAD.len()),
        dropped_frames: 0,
        gl_query_attempted: false,
        gl_limits: None,
        armed: false,
        done: false,
    }));
    let (weak, render_capture) = (app.as_weak(), capture.clone());
    if let Err(error) = app.window().set_rendering_notifier(move |state, api| {
        let mut capture = render_capture.borrow_mut();
        if !capture.gl_query_attempted
            && matches!(
                state,
                RenderingState::RenderingSetup | RenderingState::BeforeRendering
            )
        {
            capture.gl_query_attempted = true;
            capture.gl_limits = query_gl_limits(api);
        }
        if !capture.armed || capture.done {
            return;
        }
        match state {
            RenderingState::BeforeRendering => {
                let Some(app) = weak.upgrade() else { return };
                let start_us = capture.elapsed_us();
                let interval = capture
                    .previous_start_us
                    .replace(start_us)
                    .map(|last| start_us - last);
                if capture.frames.len() == MAX_FRAMES {
                    capture.dropped_frames += 1;
                    capture.pending = None;
                    return;
                }
                let f = app.global::<Frame>();
                capture.pending = Some(Sample {
                    start_us,
                    start_interval_us: interval,
                    renderer_callback_wall_us: 0,
                    phase: capture.phase,
                    tab: f.get_active_tab(),
                    photo: f.get_photo_index(),
                });
            }
            RenderingState::AfterRendering => {
                if let Some(mut sample) = capture.pending.take() {
                    sample.renderer_callback_wall_us = capture.elapsed_us() - sample.start_us;
                    capture.frames.push(sample);
                }
            }
            _ => {}
        }
    }) {
        eprintln!("[metrics] rendering notifier unavailable: {error:?}");
        return None;
    }
    let warmup = Timer::default();
    let finish = Rc::new(Timer::default());
    let finish_capture = finish.clone();
    let start_capture = move |app: &AppWindow| {
        let capture = capture.clone();
        let output = output.clone();
        let finish_capture = finish_capture.clone();
        if benchmark {
            app.global::<Frame>().set_photo_index(0);
        }
        let expected_photos = crate::photos::count();
        let initial_photo_count = app.global::<Frame>().get_photos().row_count();
        let initial_photo_index = app.global::<Frame>().get_photo_index();
        let slideshow_paused = app.global::<Frame>().get_benchmark_mode();
        let invalid_reason = if benchmark && initial_photo_count != expected_photos {
            Some("warmup_photo_count_mismatch")
        } else if benchmark && !slideshow_paused {
            Some("automatic_slideshow_not_paused")
        } else {
            None
        };
        let valid = invalid_reason.is_none();
        let initial_process = process_sample();
        let size = app.window().size();
        let scale_factor = app.window().scale_factor();
        let frame = app.global::<Frame>();
        let photo_card_width = frame.get_photo_card_width();
        let photo_card_height = frame.get_photo_card_height();
        let first_photo_size = frame
            .get_photos()
            .row_data(0)
            .map(|photo| photo.image.size());
        {
            let mut capture = capture.borrow_mut();
            capture.origin = Instant::now();
            capture.frames.clear();
            capture.pending = None;
            capture.previous_start_us = None;
            capture.actions.clear();
            capture.dropped_frames = 0;
            capture.phase = "manual";
            capture.armed = true;
        }
        if !valid {
            eprintln!("[metrics] invalid benchmark: {invalid_reason:?}; loaded {initial_photo_count}/{expected_photos} photos");
        }
        if benchmark {
            for &(scheduled_s, phase, action) in WORKLOAD {
                if scheduled_s == 0 {
                    perform(app, &capture, scheduled_s, phase, action);
                } else {
                    let (weak, capture) = (app.as_weak(), capture.clone());
                    Timer::single_shot(Duration::from_secs(scheduled_s), move || {
                        if let Some(app) = weak.upgrade() {
                            perform(&app, &capture, scheduled_s, phase, action);
                        }
                    });
                }
            }
        }
        finish_capture.start(TimerMode::SingleShot, Duration::from_secs(seconds), move || {
        let mut capture = capture.borrow_mut();
        capture.done = true;
        let actual_duration_us = capture.elapsed_us();
        let final_process = process_sample();
        let frames = std::mem::take(&mut capture.frames);
        let actions = std::mem::take(&mut capture.actions);
        let dropped = capture.dropped_frames;
        let gl_limits = capture.gl_limits;
        let initial_process = initial_process.clone();
        let output = output.clone();
        std::thread::spawn(move || {
            let samples: Vec<_> = frames.into_iter().map(|s| json!({
                "start_us": s.start_us, "start_interval_us": s.start_interval_us,
                "renderer_callback_wall_us": s.renderer_callback_wall_us,
                "phase": s.phase, "tab": s.tab, "photo": s.photo,
            })).collect();
            let report = json!({
                "schema": 2, "benchmark": benchmark, "requested_seconds": seconds,
                "valid": valid,
                "invalid_reason": invalid_reason,
                "warmup_seconds": if benchmark { WARMUP_SECONDS } else { 0 },
                "automatic_slideshow_paused": slideshow_paused,
                "expected_photo_count": expected_photos,
                "initial_photo_count": initial_photo_count, "initial_photo_index": initial_photo_index,
                "target_frame_interval_us": 16_667,
                "actual_duration_us": actual_duration_us, "width": size.width, "height": size.height,
                "scale_factor": scale_factor,
                "gl_max_texture_size": gl_limits.map(|limits| limits.0),
                "gl_max_renderbuffer_size": gl_limits.map(|limits| limits.1),
                "photo_card_width": photo_card_width, "photo_card_height": photo_card_height,
                "first_photo_width": first_photo_size.map(|size| size.width),
                "first_photo_height": first_photo_size.map(|size| size.height),
                "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
                "semantics": {
                    "start_us": "Monotonic microseconds since capture window started at BeforeRendering; benchmark warmup is excluded",
                    "start_interval_us": "Time between BeforeRendering callbacks, including idle and presentation waits; not presentation timestamps",
                    "renderer_callback_wall_us": "BeforeRendering to AfterRendering wall time; includes CPU work and submission/blocking, excludes pre-callback clear and subsequent presentation; not CPU time or GPU completion",
                    "process_ticks": "Linux process CPU clock ticks; convert deltas using device getconf CLK_TCK",
                    "peak_rss_kib": "Process lifetime high-water mark, which includes benchmark warmup",
                    "geometry": "Window/photo image dimensions and GL limits are physical pixels; photo card dimensions are logical pixels at capture start",
                    "overhead": "Notifier adds a pre-render flush in FemtoVG; compare equally instrumented builds. Notifier remains installed after collection stops.",
                    "actions": "Requested workload actions and observed globals, not proof of visible presentation. Automatic slideshow is paused in benchmark mode.",
                },
                "initial_process": initial_process, "final_process": final_process,
                "max_frames": MAX_FRAMES, "dropped_frames": dropped,
                "actions": actions, "frames": samples,
            });
            let write = || -> Result<(), Box<dyn std::error::Error>> {
                let file = OpenOptions::new().write(true).create_new(true).open(output)?;
                let mut writer = BufWriter::new(file);
                serde_json::to_writer(&mut writer, &report)?;
                writer.flush()?;
                Ok(())
            };
            match write() {
                Ok(()) => eprintln!("[metrics] capture written"),
                Err(error) => eprintln!("[metrics] capture write failed: {error}"),
            }
        });
    });
    };
    if benchmark {
        for (seconds, tab) in [(1, 5), (2, 0)] {
            let weak = app.as_weak();
            Timer::single_shot(Duration::from_secs(seconds), move || {
                if let Some(app) = weak.upgrade() {
                    app.global::<Frame>().set_active_tab(tab);
                }
            });
        }
        let weak = app.as_weak();
        warmup.start(
            TimerMode::SingleShot,
            Duration::from_secs(WARMUP_SECONDS),
            move || {
                if let Some(app) = weak.upgrade() {
                    start_capture(&app);
                }
            },
        );
    } else {
        start_capture(app);
    }
    Some(MetricsGuard {
        _warmup: warmup,
        _finish: finish,
    })
}

#[cfg(test)]
mod tests {
    use super::cpu_ticks;

    #[test]
    fn parses_cpu_ticks_after_a_process_name_with_spaces_and_parentheses() {
        let stat = "7 (frame (debug)) S 1 2 3 4 5 6 7 8 9 10 123 456 0 0";
        assert_eq!(cpu_ticks(stat), Some((123, 456)));
        assert_eq!(cpu_ticks("7 (frame) S 1"), None);
    }
}
