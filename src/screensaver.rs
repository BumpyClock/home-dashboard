#[cfg(all(feature = "frame", target_os = "linux"))]
use slint::TimerMode;
use slint::{ComponentHandle, Model, Timer};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use crate::{photos, AppWindow, Frame, Saver};

struct Config {
    start_active: bool,
    idle_after: Option<Duration>,
    benchmark: bool,
}

impl Config {
    fn parse(get: impl Fn(&str) -> Option<String>, idle_secs: u64) -> Self {
        let benchmark = get("FRAME_BENCHMARK").as_deref() == Some("1");
        Self {
            start_active: !benchmark && get("FRAME_SCREENSAVER").as_deref() == Some("1"),
            idle_after: (!benchmark && idle_secs > 0).then(|| Duration::from_secs(idle_secs)),
            benchmark,
        }
    }
}

fn swipe_target(index: i32, count: usize, dx: f32, dy: f32) -> Option<(i32, i32)> {
    if !dx.is_finite() || !dy.is_finite() || dx.abs() < 48.0 || dx.abs() <= dy.abs() * 1.2 {
        return None;
    }
    let count = i32::try_from(count).ok().filter(|count| *count > 1)?;
    let direction = if dx < 0.0 { 1 } else { -1 };
    Some((
        (index.rem_euclid(count) + direction).rem_euclid(count),
        direction,
    ))
}

#[cfg(all(feature = "frame", target_os = "linux"))]
const PHOTOS_LOADING_RETRY: Duration = Duration::from_secs(1);

pub struct Guard {
    app: slint::Weak<AppWindow>,
    timer: Timer,
    idle_after: Cell<Option<Duration>>,
    benchmark: bool,
}

#[cfg(all(feature = "frame", target_os = "linux"))]
pub struct ActivityObserver {
    _installed: (),
}

#[cfg(all(feature = "frame", target_os = "linux"))]
impl ActivityObserver {
    pub fn install(saver: &Rc<Guard>, power: &Rc<crate::display_power::Guard>) -> Self {
        let saver = saver.clone();
        power.on_activity(move |dismiss| saver.activity(dismiss));
        Self { _installed: () }
    }
}

impl Guard {
    pub fn set_idle_after(self: &Rc<Self>, idle_secs: u64) {
        let idle = (!self.benchmark && idle_secs > 0).then(|| Duration::from_secs(idle_secs));
        self.idle_after.set(idle);
        #[cfg(all(feature = "frame", target_os = "linux"))]
        match idle {
            Some(_) => self.arm_after_idle(),
            None => self.timer.stop(),
        }
    }

    #[cfg(all(feature = "frame", target_os = "linux"))]
    pub fn observe(self: &Rc<Self>, _observer: &ActivityObserver) {
        self.arm_after_idle();
    }

    #[cfg(all(feature = "frame", target_os = "linux"))]
    pub fn activity(self: &Rc<Self>, dismiss: bool) {
        if dismiss {
            if let Some(app) = self.app.upgrade() {
                app.global::<Saver>().set_active(false);
            }
        }
        self.arm_after_idle();
    }

    #[cfg(all(feature = "frame", target_os = "linux"))]
    fn arm_after_idle(self: &Rc<Self>) {
        let Some(idle_after) = self.idle_after.get() else {
            return;
        };
        let weak = Rc::downgrade(self);
        self.timer
            .start(TimerMode::SingleShot, idle_after, move || {
                if let Some(guard) = weak.upgrade() {
                    guard.show_or_wait_for_photos();
                }
            });
    }

    #[cfg(all(feature = "frame", target_os = "linux"))]
    fn show_or_wait_for_photos(self: &Rc<Self>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        if app.global::<Frame>().get_photos_loading() {
            self.retry_while_photos_loading();
        } else if photos::count() > 0 {
            app.global::<Saver>().set_active(true);
        }
    }

    #[cfg(all(feature = "frame", target_os = "linux"))]
    fn retry_while_photos_loading(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.timer
            .start(TimerMode::SingleShot, PHOTOS_LOADING_RETRY, move || {
                if let Some(guard) = weak.upgrade() {
                    guard.show_or_wait_for_photos();
                }
            });
    }
}

pub fn install(app: &AppWindow, idle_secs: u64) -> Rc<Guard> {
    let config = Config::parse(|name| std::env::var(name).ok(), idle_secs);
    let saver = app.global::<Saver>();
    let weak = app.as_weak();
    saver.on_demand(move || {
        if let Some(app) = weak.upgrade() {
            photos::demand(&app);
        }
    });
    let weak = app.as_weak();
    saver.on_swiped(move |dx, dy| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let frame = app.global::<Frame>();
        if let Some((index, direction)) = swipe_target(
            frame.get_photo_index(),
            frame.get_photos().row_count(),
            dx,
            dy,
        ) {
            frame.set_photo_direction(direction);
            frame.set_photo_index(index);
        }
    });
    saver.set_active(config.start_active);
    Rc::new(Guard {
        app: app.as_weak(),
        timer: Timer::default(),
        idle_after: Cell::new(config.idle_after),
        benchmark: config.benchmark,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, PlatformError, WindowAdapter};

    struct TestPlatform;

    impl Platform for TestPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer))
        }
    }

    #[test]
    fn saver_layer_demands_only_when_its_inputs_change() {
        std::thread::spawn(|| {
            slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
            let app = AppWindow::new().unwrap();
            let demands = Rc::new(Cell::new(0));
            let count = demands.clone();
            app.global::<Saver>()
                .on_demand(move || count.set(count.get() + 1));
            slint::platform::update_timers_and_animations();
            assert_eq!(demands.get(), 0);
            app.global::<Saver>().set_active(false);
            slint::platform::update_timers_and_animations();
            assert_eq!(demands.get(), 0);
            app.global::<Saver>().set_active(true);
            slint::platform::update_timers_and_animations();
            assert_eq!(demands.get(), 1);
        })
        .join()
        .unwrap();
    }

    fn config(values: &[(&str, &str)], idle_secs: u64) -> Config {
        Config::parse(
            |name| {
                values
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            },
            idle_secs,
        )
    }

    #[test]
    fn horizontal_swipes_advance_and_reverse_with_wrapping() {
        assert_eq!(swipe_target(0, 5, -80.0, 10.0), Some((1, 1)));
        assert_eq!(swipe_target(4, 5, -80.0, 10.0), Some((0, 1)));
        assert_eq!(swipe_target(3, 5, 80.0, -10.0), Some((2, -1)));
        assert_eq!(swipe_target(0, 5, 80.0, -10.0), Some((4, -1)));
    }

    #[test]
    fn taps_short_drags_and_vertical_gestures_do_not_change_photos() {
        assert_eq!(swipe_target(2, 5, 0.0, 0.0), None);
        assert_eq!(swipe_target(2, 5, -47.0, 0.0), None);
        assert_eq!(swipe_target(2, 5, -48.0, 0.0), Some((3, 1)));
        assert_eq!(swipe_target(2, 5, -50.0, 80.0), None);
        assert_eq!(swipe_target(2, 5, 60.0, 50.0), None);
    }

    #[test]
    fn canceled_nonfinite_gestures_and_empty_or_single_libraries_do_nothing() {
        assert_eq!(swipe_target(0, 0, -80.0, 0.0), None);
        assert_eq!(swipe_target(0, 1, 80.0, 0.0), None);
        assert_eq!(swipe_target(2, 5, f32::NAN, 0.0), None);
        assert_eq!(swipe_target(2, 5, -80.0, f32::NAN), None);
        assert_eq!(swipe_target(2, 5, f32::INFINITY, 0.0), None);
        assert_eq!(swipe_target(2, 5, -80.0, f32::NEG_INFINITY), None);
    }

    #[test]
    fn default_idle_waits_two_minutes_and_keeps_dashboard() {
        let config = config(&[], 120);
        assert_eq!(config.idle_after, Some(Duration::from_secs(120)));
        assert!(!config.start_active);
    }

    #[test]
    fn disabling_idle_still_allows_explicit_startup() {
        let config = config(&[("FRAME_SCREENSAVER", "1")], 0);
        assert!(config.idle_after.is_none());
        assert!(config.start_active);
    }

    #[test]
    fn benchmark_suppresses_automatic_and_startup_saver() {
        let config = config(&[("FRAME_BENCHMARK", "1"), ("FRAME_SCREENSAVER", "1")], 1);
        assert!(config.idle_after.is_none());
        assert!(!config.start_active);
    }

    #[test]
    fn custom_idle_delay_is_used() {
        assert_eq!(config(&[], 30).idle_after, Some(Duration::from_secs(30)));
    }
}
