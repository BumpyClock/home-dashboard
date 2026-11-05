use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};

struct TestPlatform(Rc<Cell<Duration>>);

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer))
    }

    fn duration_since_start(&self) -> Duration {
        self.0.get()
    }
}

struct Scene {
    app: AppWindow,
    state: State,
    clock: Rc<Cell<Duration>>,
    _directory: tempfile::TempDir,
}

impl Scene {
    fn new() -> Self {
        let clock = Rc::new(Cell::new(Duration::from_secs(1)));
        slint::platform::set_platform(Box::new(TestPlatform(clock.clone()))).unwrap();
        let app = AppWindow::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let state = State {
            settings: Cell::new(Settings::default()),
            theme_transition: Timer::default(),
            chores: RefCell::new(vec![]),
            list: RefCell::new(vec![]),
            calendar: Calendar::from_paths(directory.path().join("host.json"), None).unwrap(),
            models: Models {
                agenda_past: Rc::new(VecModel::default()),
                agenda_current: Rc::new(VecModel::default()),
                tomorrow: Rc::new(VecModel::default()),
                calendar_days: Rc::new(VecModel::default()),
                list: Rc::new(VecModel::default()),
                chores: Rc::new(VecModel::default()),
            },
            screensaver: RefCell::new(None),
        };
        slint::platform::update_timers_and_animations();
        apply_settings(&app, &state, false);
        Self {
            app,
            state,
            clock,
            _directory: directory,
        }
    }

    fn advance(&self, millis: u64) {
        self.clock
            .set(self.clock.get() + Duration::from_millis(millis));
        slint::platform::update_timers_and_animations();
    }

    fn select(&self, index: usize) {
        let mut settings = self.state.settings.get();
        settings.theme = index;
        self.state.settings.set(settings);
        apply_settings(&self.app, &self.state, true);
    }

    fn assert_palette(&self, index: usize, dip: bool) {
        assert_eq!(
            self.app.global::<Theme>().get_c(),
            settings::themes()[index].1
        );
        assert_eq!(self.app.global::<Frame>().get_dip(), dip);
        assert_eq!(
            self.app.global::<Frame>().get_theme_index(),
            self.state.settings.get().theme as i32
        );
    }
}

fn isolated(test: impl FnOnce(Scene) + Send + 'static) {
    std::thread::spawn(move || test(Scene::new()))
        .join()
        .unwrap();
}

#[test]
fn latest_theme_replaces_the_pending_palette_and_deadline() {
    isolated(|scene| {
        scene.select(1);
        scene.assert_palette(0, true);
        scene.advance(100);
        scene.select(2);
        scene.advance(110);
        scene.assert_palette(0, true);
        scene.advance(99);
        scene.assert_palette(0, true);
        scene.advance(1);
        scene.assert_palette(2, false);
        scene.advance(500);
        scene.assert_palette(2, false);
    });
}

#[test]
fn returning_to_the_visible_palette_cancels_the_pending_change_and_dip() {
    isolated(|scene| {
        scene.select(1);
        scene.advance(100);
        scene.select(0);
        scene.assert_palette(0, false);
        scene.advance(500);
        scene.assert_palette(0, false);
    });
}

#[test]
fn turning_motion_off_applies_latest_palette_and_cancels_pending_change() {
    isolated(|scene| {
        scene.select(1);
        scene.advance(100);
        let mut settings = scene.state.settings.get();
        settings.theme = 2;
        settings.motion = 2;
        scene.state.settings.set(settings);
        apply_settings(&scene.app, &scene.state, true);
        assert!(!scene.app.global::<Theme>().get_motion());
        assert!(!scene.app.global::<Theme>().get_ambient());
        assert_eq!(scene.app.global::<Frame>().get_motion_index(), 2);
        scene.assert_palette(2, false);
        scene.advance(500);
        scene.assert_palette(2, false);
    });
}

#[test]
fn immediate_application_cancels_the_pending_palette_and_dip() {
    isolated(|scene| {
        scene.select(1);
        scene.advance(100);
        let mut settings = scene.state.settings.get();
        settings.theme = 2;
        scene.state.settings.set(settings);
        apply_settings(&scene.app, &scene.state, false);
        scene.assert_palette(2, false);
        scene.advance(500);
        scene.assert_palette(2, false);
    });
}

#[test]
fn unrelated_preferences_apply_live_and_replace_the_pending_theme_deadline() {
    isolated(|scene| {
        scene.select(1);
        scene.advance(100);
        let mut settings = scene.state.settings.get();
        settings.clock24 = false;
        settings.body_font = 1;
        settings.display_font = 3;
        settings.scale = 1;
        scene.state.settings.set(settings);
        apply_settings(&scene.app, &scene.state, true);
        let theme = scene.app.global::<Theme>();
        assert!(!scene.app.global::<Frame>().get_clock24());
        assert_eq!(theme.get_font_body(), "Atkinson Hyperlegible");
        assert_eq!(theme.get_font_display(), "Atkinson Hyperlegible");
        assert_eq!(theme.get_scale(), 1.12);
        scene.advance(110);
        scene.assert_palette(0, true);
        scene.advance(100);
        scene.assert_palette(1, false);
        assert_eq!(theme.get_font_body(), "Atkinson Hyperlegible");
        assert!(!scene.app.global::<Frame>().get_clock24());
    });
}

#[test]
fn pending_theme_does_not_keep_a_disposed_window_alive() {
    isolated(|scene| {
        scene.select(1);
        let weak = scene.app.as_weak();
        let Scene {
            app,
            state: _state,
            clock,
            _directory,
        } = scene;
        drop(app);
        assert!(weak.upgrade().is_none());
        clock.set(clock.get() + Duration::from_millis(500));
        slint::platform::update_timers_and_animations();
        assert!(weak.upgrade().is_none());
    });
}
