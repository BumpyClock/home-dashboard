use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::ComponentHandle;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

slint::slint! {
    import { PhotoStage } from "../ui/components.slint";
    import { Frame } from "../ui/data.slint";
    import { Theme } from "../ui/theme.slint";
    export { Frame, Theme }

    export component StageWindow inherits Window {
        in property <bool> ready: true;
        out property <int> current: stage.current-index;
        out property <int> previous: stage.previous-index;
        out property <bool> moving: stage.moving;
        stage := PhotoStage {
            width: 100px;
            height: 100px;
            running: true;
            autoplay: false;
            slide: true;
            interval: 120s;
            ready: root.ready;
        }
    }
}

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
    window: StageWindow,
    clock: Rc<Cell<Duration>>,
}

impl Scene {
    fn new() -> Self {
        let clock = Rc::new(Cell::new(Duration::from_secs(1)));
        slint::platform::set_platform(Box::new(TestPlatform(clock.clone()))).unwrap();
        let window = StageWindow::new().unwrap();
        let mut scene = Self { window, clock };
        scene.advance(Duration::from_millis(5));
        scene
    }

    fn advance(&mut self, duration: Duration) {
        let end = self.clock.get() + duration;
        while self.clock.get() < end {
            self.clock
                .set((self.clock.get() + Duration::from_millis(5)).min(end));
            slint::platform::update_timers_and_animations();
        }
    }

    fn request(&mut self, index: i32) {
        self.window.global::<Frame>().set_photo_index(index);
        self.advance(Duration::from_millis(10));
    }

    fn ready(&mut self, ready: bool) {
        self.window.set_ready(ready);
        self.advance(Duration::from_millis(10));
    }

    fn assert_idle(&self, current: i32) {
        assert_eq!(self.window.get_current(), current);
        assert_eq!(self.window.get_previous(), -1);
        assert!(!self.window.get_moving());
    }
}

fn isolated(test: impl FnOnce(&mut Scene) + Send + 'static) {
    std::thread::spawn(move || test(&mut Scene::new()))
        .join()
        .unwrap();
}

#[test]
fn delayed_destination_keeps_the_visible_photo_until_pixels_arrive() {
    isolated(|scene| {
        scene.ready(false);
        scene.request(1);
        scene.advance(Duration::from_secs(2));
        scene.assert_idle(0);
        scene.ready(true);
        assert_eq!(scene.window.get_current(), 1);
        assert_eq!(scene.window.get_previous(), 0);
        assert!(scene.window.get_moving());
        scene.advance(Duration::from_secs(2));
        scene.assert_idle(1);
    });
}

#[test]
fn wraparound_waits_for_the_first_photos_pixels() {
    isolated(|scene| {
        scene.request(65);
        scene.advance(Duration::from_secs(2));
        scene.assert_idle(65);
        scene.ready(false);
        scene.request(0);
        scene.advance(Duration::from_secs(2));
        scene.assert_idle(65);
        scene.ready(true);
        assert_eq!(scene.window.get_current(), 0);
        assert_eq!(scene.window.get_previous(), 65);
        scene.advance(Duration::from_secs(2));
        scene.assert_idle(0);
    });
}

#[test]
fn rapid_requests_while_loading_coalesce_to_the_latest_destination() {
    isolated(|scene| {
        scene.ready(false);
        scene.request(1);
        scene.request(2);
        scene.advance(Duration::from_secs(2));
        scene.assert_idle(0);
        scene.ready(true);
        assert_eq!(scene.window.get_current(), 2);
        assert_eq!(scene.window.get_previous(), 0);
        scene.advance(Duration::from_secs(2));
        scene.assert_idle(2);
    });
}

#[test]
fn disabling_motion_still_waits_for_destination_pixels() {
    isolated(|scene| {
        scene.ready(false);
        scene.request(1);
        scene.window.global::<Theme>().set_motion(false);
        scene.advance(Duration::from_secs(2));
        scene.assert_idle(0);
        scene.ready(true);
        scene.assert_idle(1);
    });
}
