//! Screensaver startup and idle policy. Human input comes from the existing
//! frame display observer; rotation only wakes the backlight.

use slint::{ComponentHandle, Model, Timer, TimerMode};
use std::rc::Rc;
use std::time::Duration;

use crate::{photos, AppWindow, Frame, Saver};

struct Config {
    start_active: bool,
    idle_after: Option<Duration>,
}

impl Config {
    fn parse(get: impl Fn(&str) -> Option<String>) -> Self {
        let benchmark = get("FRAME_BENCHMARK").as_deref() == Some("1");
        let seconds = get("FRAME_SCREENSAVER_IDLE")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(120);
        Self {
            start_active: !benchmark && get("FRAME_SCREENSAVER").as_deref() == Some("1"),
            idle_after: (!benchmark && seconds > 0).then(|| Duration::from_secs(seconds)),
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

pub struct Guard {
    app: slint::Weak<AppWindow>,
    timer: Timer,
    idle_after: Option<Duration>,
}

impl Guard {
    /// Arm only after a human input observer has been installed.
    pub fn observe(self: &Rc<Self>) {
        if let Some(delay) = self.idle_after {
            self.arm(delay);
        }
    }

    fn arm(self: &Rc<Self>, delay: Duration) {
        let weak = Rc::downgrade(self);
        self.timer.start(TimerMode::SingleShot, delay, move || {
            let Some(guard) = weak.upgrade() else {
                return;
            };
            let Some(app) = guard.app.upgrade() else {
                return;
            };
            if app.global::<Frame>().get_photos_loading() {
                // A slow initial metadata scan must not consume the only idle
                // deadline. Retry readiness without restarting the human wait.
                guard.arm(Duration::from_secs(1));
            } else if photos::count() > 0 {
                app.global::<Saver>().set_active(true);
            }
        });
    }

    /// Raw pointer input rearms; Slint's saver click handler owns pointer dismissal.
    pub fn activity(self: &Rc<Self>, dismiss: bool) {
        if dismiss {
            if let Some(app) = self.app.upgrade() {
                app.global::<Saver>().set_active(false);
            }
        }
        self.observe();
    }
}

pub fn install(app: &AppWindow) -> Rc<Guard> {
    let config = Config::parse(|name| std::env::var(name).ok());
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
    // SaverLayer's initial demand can precede installation of this callback.
    photos::demand(app);
    Rc::new(Guard {
        app: app.as_weak(),
        timer: Timer::default(),
        idle_after: config.idle_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(values: &[(&str, &str)]) -> Config {
        Config::parse(|name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        })
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
        let config = config(&[]);
        assert_eq!(config.idle_after, Some(Duration::from_secs(120)));
        assert!(!config.start_active);
    }

    #[test]
    fn disabling_idle_still_allows_explicit_startup() {
        let config = config(&[("FRAME_SCREENSAVER_IDLE", "0"), ("FRAME_SCREENSAVER", "1")]);
        assert!(config.idle_after.is_none());
        assert!(config.start_active);
    }

    #[test]
    fn benchmark_suppresses_automatic_and_startup_saver() {
        let config = config(&[
            ("FRAME_BENCHMARK", "1"),
            ("FRAME_SCREENSAVER", "1"),
            ("FRAME_SCREENSAVER_IDLE", "1"),
        ]);
        assert!(config.idle_after.is_none());
        assert!(!config.start_active);
    }

    #[test]
    fn custom_idle_and_invalid_idle_use_expected_delays() {
        assert_eq!(
            config(&[("FRAME_SCREENSAVER_IDLE", "30")]).idle_after,
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            config(&[("FRAME_SCREENSAVER_IDLE", "-1")]).idle_after,
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            config(&[("FRAME_SCREENSAVER_IDLE", "18446744073709551616")]).idle_after,
            Some(Duration::from_secs(120))
        );
    }
}
