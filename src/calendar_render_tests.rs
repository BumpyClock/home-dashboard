use super::*;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{
    Key, Platform, PlatformError, PointerEventButton, WindowAdapter, WindowEvent,
};
use slint::{LogicalPosition, LogicalSize, Rgb8Pixel};

mod privacy_ui {
    slint::slint! {
        import { CalendarPrivacyChoice } from "../ui/settings.slint";
        import { Frame } from "../ui/data.slint";
        import { Theme } from "../ui/theme.slint";
        import { Space } from "../ui/tokens.slint";
        export { Frame, Theme }
        export component PrivacyWindow inherits Window {
            background: Theme.c.bg;
            forward-focus: control;
            control := CalendarPrivacyChoice { x: Space.gutter; y: Space.gutter; width: parent.width - 2 * Space.gutter; }
        }
    }
}

#[test]
fn privacy_option_supports_touch_keyboard_and_large_text() {
    for (width, theme) in [(560, 0), (800, 4)] {
        std::thread::spawn(move || {
            let clock = Rc::new(Cell::new(Duration::from_secs(1)));
            let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
            slint::platform::set_platform(Box::new(RenderPlatform {
                clock: clock.clone(),
                adapter: adapter.clone(),
            }))
            .unwrap();
            let window = privacy_ui::PrivacyWindow::new().unwrap();
            let colors = settings::themes()[theme].1.clone();
            let mut palette = window.global::<privacy_ui::Theme>().get_c();
            palette.bg = colors.bg;
            palette.surface = colors.surface;
            palette.ink = colors.ink;
            palette.muted = colors.muted;
            palette.accent = colors.accent;
            window.global::<privacy_ui::Theme>().set_c(palette);
            window.global::<privacy_ui::Theme>().set_scale(1.25);
            let weak = window.as_weak();
            window
                .global::<privacy_ui::Frame>()
                .on_pick_hide_host_calendar_when_locked(move |on| {
                    weak.upgrade()
                        .unwrap()
                        .global::<privacy_ui::Frame>()
                        .set_hide_host_calendar_when_locked(on);
                });
            window.show().unwrap();
            window
                .window()
                .set_size(LogicalSize::new(width as f32, 260.0));
            window
                .window()
                .dispatch_event(WindowEvent::WindowActiveChanged(true));
            let position = LogicalPosition::new(width as f32 / 2.0, 60.0);
            window.window().dispatch_event(WindowEvent::PointerPressed {
                position,
                button: PointerEventButton::Left,
            });
            window
                .window()
                .dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Left,
                });
            assert!(window
                .global::<privacy_ui::Frame>()
                .get_hide_host_calendar_when_locked());
            clock.set(clock.get() + Duration::from_millis(20));
            slint::platform::update_timers_and_animations();
            let mut pixels = vec![Rgb8Pixel::default(); width as usize * 260];
            window.window().request_redraw();
            assert!(adapter.draw_if_needed(|renderer| {
                renderer.render(&mut pixels, width as usize);
            }));
            capture(&format!("privacy-setting-{width}"), width, 260, &pixels);
            window.window().dispatch_event(WindowEvent::KeyPressed {
                text: Key::Space.into(),
            });
            window.window().dispatch_event(WindowEvent::KeyReleased {
                text: Key::Space.into(),
            });
            assert!(!window
                .global::<privacy_ui::Frame>()
                .get_hide_host_calendar_when_locked());
        })
        .join()
        .unwrap();
    }
}

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

fn fixture_days(clock24: bool) -> Vec<CalendarDay> {
    (0..7).map(|offset| {
        let date = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap() + CDur::days(offset);
        let count = if offset == 0 { 3 } else { 4 };
        CalendarDay {
            heading: s(match offset { 0 => "Today".into(), 1 => "Tomorrow".into(), _ => date.format("%A").to_string() }),
            date: s(date.format("%A, %-d %B").to_string()),
            today: offset == 0,
            summary: s(plural(count, "event")),
            events: ModelRc::new(VecModel::from((0..count).map(|index| {
                let time = date.and_hms_opt(9 + index as u32, 30, 0).unwrap();
                AgendaRow {
                    time: s(if index == 0 { "All day".into() } else { fmt_time(time, clock24) }),
                    title: s(if index == 0 { "School holiday".into() }
                        else if offset == 0 && index == 2 { "A longer project review title that wraps naturally without hiding the location or the next appointment".into() }
                        else { format!("Design review {}", offset * 4 + index as i64) }),
                    meta: s(if index == 0 { "".into() } else if offset == 0 && index == 1 {
                        format!("Happening now · Office · {} to {}", fmt_time(time, clock24), fmt_time(time + CDur::hours(1), clock24))
                    } else { format!("Workshop room, North building · {} to {}", fmt_time(time, clock24), fmt_time(time + CDur::hours(1), clock24)) }),
                    next: offset == 0 && index == 1,
                    ..Default::default()
                }
            }).collect::<Vec<_>>())),
        }
    }).collect()
}

fn capture(label: &str, width: u32, height: u32, pixels: &[Rgb8Pixel]) {
    let Some(path) = std::env::var_os("FRAME_CALENDAR_RENDER_DIR") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    std::fs::create_dir_all(&path).unwrap();
    let bytes: Vec<_> = pixels
        .iter()
        .flat_map(|pixel| [pixel.r, pixel.g, pixel.b])
        .collect();
    image::codecs::jpeg::JpegEncoder::new_with_quality(
        std::fs::File::create(path.join(format!("{label}.jpg"))).unwrap(),
        98,
    )
    .encode(&bytes, width, height, image::ExtendedColorType::Rgb8)
    .unwrap();
}

#[test]
fn calendar_text_and_focus_roles_have_contrast_in_every_theme() {
    let luminance = |color: Color| {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.red())
            + 0.7152 * channel(color.green())
            + 0.0722 * channel(color.blue())
    };
    let contrast = |a: Color, b: Color| {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    };
    for (name, palette) in settings::themes() {
        for background in [palette.bg, palette.surface, palette.card_hi] {
            assert!(
                contrast(palette.ink, background) >= 4.5,
                "calendar primary text in {name}"
            );
            assert!(
                contrast(palette.muted, background) >= 4.5,
                "calendar secondary text in {name}"
            );
        }
        assert!(
            contrast(palette.accent, palette.bg) >= 3.0,
            "Today heading in {name}"
        );
        assert!(
            contrast(palette.accent, palette.surface) >= 3.0,
            "keyboard focus in {name}"
        );
    }
}

#[test]
fn upcoming_calendar_renders_and_preserves_keyboard_scroll_across_tabs() {
    for (label, width, height, theme, scale, clock24) in [
        ("portrait", 800, 1280, 0, 1.0, true),
        ("landscape", 1280, 800, 0, 1.0, true),
        ("night-large-text", 800, 1280, 4, 1.25, false),
        ("small-large-text", 560, 800, 0, 1.25, false),
    ] {
        std::thread::spawn(move || {
            let clock = Rc::new(Cell::new(Duration::from_secs(1)));
            let adapter = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
            slint::platform::set_platform(Box::new(RenderPlatform {
                clock: clock.clone(),
                adapter: adapter.clone(),
            }))
            .unwrap();
            let app = AppWindow::new().unwrap();
            let palette = app.global::<Theme>();
            palette.set_c(settings::themes()[theme].1.clone());
            palette.set_scale(scale);
            palette.set_motion(false);
            palette.set_ambient(false);
            let f = app.global::<Frame>();
            f.set_active_tab(1);
            f.set_clock24(clock24);
            f.set_calendar_synced(true);
            f.set_calendar_event_count(27);
            f.set_calendar_summary(s("Test data · 27 events · Next 7 days"));
            f.set_calendar_updated(s("Test data · Last synced at 10:42 am"));
            f.set_calendar_days(ModelRc::new(VecModel::from(fixture_days(clock24))));
            app.show().unwrap();
            app.window()
                .set_size(LogicalSize::new(width as f32, height as f32));
            app.window()
                .dispatch_event(WindowEvent::WindowActiveChanged(true));
            let render = || {
                clock.set(clock.get() + Duration::from_millis(20));
                slint::platform::update_timers_and_animations();
                let mut pixels = vec![Rgb8Pixel::default(); (width * height) as usize];
                app.window().request_redraw();
                assert!(adapter.draw_if_needed(|renderer| {
                    renderer.render(&mut pixels, width as usize);
                }));
                pixels
            };
            let key = |key: Key| {
                let text: SharedString = key.into();
                app.window()
                    .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
                app.window()
                    .dispatch_event(WindowEvent::KeyReleased { text });
            };
            let top = render();
            capture(label, width, height, &top);
            key(Key::PageDown);
            let below = render();
            assert!(top != below, "PageDown did not scroll {label}");
            f.set_active_tab(0);
            render();
            f.set_active_tab(1);
            let returned = render();
            assert!(
                below == returned,
                "Changing tabs lost calendar position in {label}"
            );
            key(Key::Home);
            assert!(top == render(), "Home did not return to today in {label}");
            app.window().dispatch_event(WindowEvent::PointerScrolled {
                position: LogicalPosition::new(width as f32 / 2.0, height as f32 / 2.0),
                delta_x: 0.0,
                delta_y: -300.0,
            });
            assert!(top != render(), "Mouse wheel did not scroll {label}");
            key(Key::Home);
            assert!(
                top == render(),
                "Home after wheel did not restore the first rows"
            );
            let down = LogicalPosition::new(width as f32 / 2.0, height as f32 * 0.75);
            let up = LogicalPosition::new(width as f32 / 2.0, height as f32 * 0.45);
            app.window().dispatch_event(WindowEvent::PointerPressed {
                position: down,
                button: PointerEventButton::Left,
            });
            clock.set(clock.get() + Duration::from_millis(16));
            app.window()
                .dispatch_event(WindowEvent::PointerMoved { position: up });
            clock.set(clock.get() + Duration::from_millis(16));
            app.window().dispatch_event(WindowEvent::PointerReleased {
                position: up,
                button: PointerEventButton::Left,
            });
            assert!(top != render(), "Touch drag did not scroll {label}");
            key(Key::Tab);
            key(Key::Return);
            let restored = render();
            let content_matches = (220..height.saturating_sub(100)).all(|y| {
                let start = (y * width) as usize;
                top[start..start + width as usize] == restored[start..start + width as usize]
            });
            assert!(
                content_matches,
                "Today button did not restore the first rows in {label}"
            );
            f.set_calendar_event_count(0);
            f.set_calendar_days(ModelRc::default());
            f.set_calendar_summary(s("Test data · 0 events · Next 7 days"));
            let empty = render();
            capture(&format!("{label}-empty"), width, height, &empty);
            f.set_calendar_synced(false);
            f.set_calendar_summary(s("Upcoming events"));
            f.set_calendar_updated(s(""));
            let unsynced = render();
            assert!(
                empty != unsynced,
                "Empty and not-synced states look identical"
            );
            capture(&format!("{label}-not-synced"), width, height, &unsynced);
            f.set_calendar_synced(true);
            f.set_host_calendar_hidden(true);
            f.set_host_calendar_privacy(s("Host calendar hidden · Computer locked"));
            let locked = render();
            assert!(
                locked != empty && locked != unsynced,
                "privacy state must not look like an empty calendar"
            );
            capture(&format!("{label}-locked"), width, height, &locked);
        })
        .join()
        .unwrap();
    }
}
