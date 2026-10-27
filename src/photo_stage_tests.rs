use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

slint::slint! {
    import { PhotoStage, PhotoCard } from "../ui/components.slint";
    import { Frame, Photo } from "../ui/data.slint";
    import { Theme } from "../ui/theme.slint";
    import { Saver } from "../ui/saver.slint";
    import { SaverLayer } from "../ui/screensaver.slint";
    export { Frame, Theme, Photo, Saver }

    export component CardWindow inherits Window {
        width: 120px;
        height: 120px;
        in property <bool> saver;
        if !root.saver: PhotoCard { width: parent.width; height: parent.height; }
        if root.saver: SaverLayer { width: parent.width; height: parent.height; }
    }

    export component StageWindow inherits Window {
        in property <bool> ready: true;
        out property <int> current: stage.current-index;
        out property <int> previous: stage.previous-index;
        out property <bool> moving: stage.moving;
        out property <int> slot-0: stage.slot-0-index;
        out property <int> slot-1: stage.slot-1-index;
        out property <bool> current-slot-1: stage.current-slot-1;
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

struct RenderPlatform {
    clock: Rc<Cell<Duration>>,
    adapter: Rc<MinimalSoftwareWindow>,
}

impl Platform for RenderPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.adapter.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.clock.get()
    }
}

impl Platform for TestPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer))
    }

    fn duration_since_start(&self) -> Duration {
        self.0.get()
    }
}

type StageEvent = (i32, i32, i32, i32, bool);

struct Scene {
    window: StageWindow,
    clock: Rc<Cell<Duration>>,
    events: Rc<RefCell<Vec<StageEvent>>>,
}

impl Scene {
    fn new() -> Self {
        let clock = Rc::new(Cell::new(Duration::from_secs(1)));
        slint::platform::set_platform(Box::new(TestPlatform(clock.clone()))).unwrap();
        let window = StageWindow::new().unwrap();
        let events = Rc::new(RefCell::new(Vec::new()));
        let observed = events.clone();
        window.global::<Frame>().on_trace_photo_stage(
            move |_, event, requested, current, previous, ready| {
                observed
                    .borrow_mut()
                    .push((event, requested, current, previous, ready));
            },
        );
        let mut scene = Self {
            window,
            clock,
            events,
        };
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

#[test]
fn transition_reports_wait_start_and_completion() {
    isolated(|scene| {
        scene.ready(false);
        scene.request(1);
        assert!(scene.events.borrow().contains(&(0, 1, 0, -1, false)));
        scene.ready(true);
        assert!(scene.events.borrow().contains(&(2, 1, 1, 0, true)));
        scene.advance(Duration::from_secs(2));
        assert!(scene.events.borrow().contains(&(5, 1, 1, 0, true)));
    });
}

#[test]
fn disabling_motion_reports_cancellation() {
    isolated(|scene| {
        scene.request(1);
        scene.window.global::<Theme>().set_motion(false);
        scene.advance(Duration::from_millis(10));
        assert!(scene.events.borrow().iter().any(|e| e.0 == 3 && e.2 == 1));
    });
}

#[test]
fn transition_keeps_the_outgoing_photo_in_its_existing_slot() {
    isolated(|scene| {
        assert_eq!(scene.window.get_slot_0(), 0);
        scene.request(1);
        assert_eq!(
            (scene.window.get_slot_0(), scene.window.get_slot_1()),
            (0, 1)
        );
        assert!(scene.window.get_current_slot_1());
        scene.advance(Duration::from_secs(2));
        scene.request(2);
        assert_eq!(
            (scene.window.get_slot_0(), scene.window.get_slot_1()),
            (2, 1)
        );
        assert!(!scene.window.get_current_slot_1());
        scene.advance(Duration::from_secs(2));
        scene.assert_idle(2);
    });
}

#[test]
fn snapping_updates_the_current_slot_before_the_next_transition() {
    isolated(|scene| {
        scene.window.global::<Theme>().set_motion(false);
        scene.request(1);
        scene.assert_idle(1);
        assert_eq!(scene.window.get_slot_0(), 1);
        scene.window.global::<Theme>().set_motion(true);
        scene.request(2);
        assert_eq!(
            (scene.window.get_slot_0(), scene.window.get_slot_1()),
            (1, 2)
        );
        assert!(scene.window.get_current_slot_1());
    });
}

#[test]
fn recreating_home_closes_its_transition_trace_before_idle() {
    const CHILD: &str = "FRAME_STAGE_TRACE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let _guard = frame_trace::init_from_env();
        assert!(frame_trace::enabled());
        let clock = Rc::new(Cell::new(Duration::from_secs(1)));
        let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(RenderPlatform {
            clock: clock.clone(),
            adapter: adapter.clone(),
        }))
        .unwrap();
        let app = crate::AppWindow::new().unwrap();
        let frame = app.global::<crate::Frame>();
        let image =
            slint::Image::from_rgb8(slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(2, 2));
        frame.set_photos(slint::ModelRc::new(slint::VecModel::from(vec![
            crate::Photo {
                image,
                ..Default::default()
            };
            2
        ])));
        crate::photos::trace::install_ui(&app);
        app.window()
            .set_size(slint::LogicalSize::new(1280.0, 800.0));
        app.show().unwrap();
        let advance = |ms| {
            for _ in 0..ms / 5 {
                clock.set(clock.get() + Duration::from_millis(5));
                slint::platform::update_timers_and_animations();
            }
            let mut buffer = vec![slint::Rgb8Pixel::default(); 1280 * 800];
            adapter.draw_if_needed(|renderer| {
                renderer.render(&mut buffer, 1280);
            });
            clock.set(clock.get() + Duration::from_millis(5));
            slint::platform::update_timers_and_animations();
        };
        advance(20);
        frame.set_photo_index(1);
        advance(20);
        assert_eq!(app.global::<crate::Saver>().get_card_previous(), 0);
        frame.set_orientation_degrees(90);
        advance(20);
        assert_eq!(app.global::<crate::Saver>().get_card_previous(), -1);
        advance(2000);
        frame.set_photo_index(0);
        advance(20);
        assert_eq!(app.global::<crate::Saver>().get_card_previous(), 1);
        advance(2000);
        return;
    }

    let dir = std::env::temp_dir().join(format!(
        "frame-stage-trace-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let output = dir.join("trace.jsonl");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "photo_stage_tests::recreating_home_closes_its_transition_trace_before_idle",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("FRAME_TRACE_OUTPUT", &output)
        .env("FRAME_TRACE_SECONDS", "10")
        .env_remove("FRAME_METRICS_OUTPUT")
        .status()
        .unwrap();
    assert!(status.success());
    let wire = std::fs::read_to_string(&output).unwrap();
    let boundaries: Vec<serde_json::Value> = wire
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| {
            matches!(
                event["name"].as_str(),
                Some("photo_transition_start" | "photo_transition_cancel" | "photo_transition_end")
            )
        })
        .collect();
    assert_eq!(
        boundaries
            .iter()
            .map(|event| event["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "photo_transition_start",
            "photo_transition_cancel",
            "photo_transition_start",
            "photo_transition_end"
        ]
    );
    assert_eq!(boundaries[0]["op_id"], boundaries[1]["op_id"]);
    assert_eq!(boundaries[2]["op_id"], boundaries[3]["op_id"]);
    assert_ne!(boundaries[0]["op_id"], boundaries[2]["op_id"]);
    std::fs::remove_file(output).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn photo_surfaces_fade_in_both_slot_directions_and_keep_overlays_on_top() {
    for saver in [false, true] {
        std::thread::spawn(move || {
            use slint::{Image, ModelRc, PhysicalSize, Rgb8Pixel, SharedPixelBuffer, VecModel};

            let clock = Rc::new(Cell::new(Duration::from_secs(1)));
            let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
            slint::platform::set_platform(Box::new(RenderPlatform {
                clock: clock.clone(),
                adapter: adapter.clone(),
            }))
            .unwrap();
            let window = CardWindow::new().unwrap();
            let frame = window.global::<Frame>();
            window.set_saver(saver);
            window.global::<Saver>().set_active(saver);
            frame.set_clock("".into());
            frame.set_temp("".into());
            frame.set_cond("".into());
            frame.set_photo_transition_slide(false);
            frame.set_photo_scrim_baked(true);
            let image = |r, g, b| {
                let mut pixels = SharedPixelBuffer::<Rgb8Pixel>::new(2, 2);
                pixels.make_mut_slice().fill(Rgb8Pixel { r, g, b });
                Image::from_rgb8(pixels)
            };
            let photos: Vec<_> = [(255, 0, 0), (0, 0, 255), (0, 255, 0)]
                .into_iter()
                .map(|(r, g, b)| {
                    let image = image(r, g, b);
                    Photo {
                        full: image.clone(),
                        image,
                        ..Default::default()
                    }
                })
                .collect();
            frame.set_photos(ModelRc::from(Rc::new(VecModel::from(photos))));
            adapter.set_size(PhysicalSize::new(120, 120));
            window.show().unwrap();
            let advance = |ms| {
                clock.set(clock.get() + Duration::from_millis(ms));
                slint::platform::update_timers_and_animations();
            };
            let pixel = || {
                let mut buffer = vec![Rgb8Pixel::default(); 120 * 120];
                window.window().request_redraw();
                assert!(adapter.draw_if_needed(|renderer| {
                    renderer.render(&mut buffer, 120);
                }));
                if !saver {
                    let bg = window.global::<Theme>().get_c().bg;
                    let corner = Rgb8Pixel {
                        r: bg.red(),
                        g: bg.green(),
                        b: bg.blue(),
                    };
                    for index in [0, 119, 119 * 120, 120 * 120 - 1] {
                        assert_eq!(buffer[index], corner, "card corner at {index}");
                    }
                }
                buffer[30 * 120 + 60]
            };
            advance(10);
            let red = pixel();
            assert!(red.r > 240 && red.g < 10 && red.b < 10, "initial: {red:?}");
            if saver {
                frame.set_night_active(true);
                advance(10);
                let dimmed = pixel();
                assert!(
                    dimmed.r > 50 && dimmed.r < 180 && dimmed.g < 10 && dimmed.b < 10,
                    "night overlay: {dimmed:?}"
                );
                frame.set_night_active(false);
                advance(10);
            }
            frame.set_photo_index(1);
            advance(10);
            pixel();
            advance(250);
            let midway = pixel();
            assert!(
                midway.r > 20 && midway.b > 20 && midway.g < 10,
                "red-blue fade: {midway:?}"
            );
            advance(300);
            let blue = pixel();
            assert!(blue.b > 240 && blue.r < 10 && blue.g < 10, "blue: {blue:?}");
            frame.set_photo_index(2);
            advance(10);
            pixel();
            advance(250);
            let midway = pixel();
            assert!(
                midway.b > 20 && midway.g > 20 && midway.r < 10,
                "blue-green fade: {midway:?}"
            );
            advance(300);
            let green = pixel();
            assert!(
                green.g > 240 && green.r < 10 && green.b < 10,
                "green: {green:?}"
            );
        })
        .join()
        .unwrap();
    }
}
