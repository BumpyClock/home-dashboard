use super::*;
use calendar_sync::{Event as WireEvent, EventTime, Snapshot, Source};
use chrono::TimeZone;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::Model;
use std::time::Instant;

struct Headless;
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer))
    }
}

fn wait_update(calendar: &Calendar) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !calendar.poll() {
        assert!(
            Instant::now() < deadline,
            "frame did not reload calendar files"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn status_packet(path: &std::path::Path, state: host_state::LockState) {
    let mut bytes = host_state::Status::new(state).encode().unwrap();
    bytes.push(b'\n');
    host_state::receive(&bytes[..], Vec::new(), path).unwrap();
}

#[test]
fn host_lock_hides_every_host_calendar_surface_without_erasing_direct_events() {
    std::thread::spawn(|| {
        use host_state::LockState;
        slint::platform::set_platform(Box::new(Headless)).unwrap();
        let app = AppWindow::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let host_path = directory.path().join("host.json");
        let direct_path = directory.path().join("direct.json");
        let status_path = host_state::path_for_calendar(&host_path);
        let now = Utc::now();
        let public = WireEvent {
            id: "public".into(),
            uid: Some("shared".into()),
            title: "Direct calendar".into(),
            detail: "Public location".into(),
            time: EventTime::Timed {
                start: now - CDur::minutes(5),
                end: now + CDur::minutes(20),
            },
        };
        let private = WireEvent {
            id: "private".into(),
            uid: None,
            title: "SECRET work meeting".into(),
            detail: "SECRET location".into(),
            time: EventTime::Timed {
                start: now - CDur::minutes(10),
                end: now + CDur::minutes(20),
            },
        };
        Snapshot::new(vec![Source {
            id: "direct".into(),
            events: vec![public.clone()],
        }])
        .save(&direct_path)
        .unwrap();
        Snapshot::new(vec![Source {
            id: "host".into(),
            events: vec![
                private.clone(),
                WireEvent {
                    id: "host-copy".into(),
                    title: "SECRET duplicate".into(),
                    ..public
                },
                WireEvent {
                    id: "tomorrow-private".into(),
                    time: EventTime::Timed {
                        start: now + CDur::days(1),
                        end: now + CDur::days(1) + CDur::minutes(30),
                    },
                    ..private
                },
            ],
        }])
        .save(&host_path)
        .unwrap();
        let st =
            test_state(Calendar::from_paths(host_path.clone(), Some(direct_path.clone())).unwrap());
        let mut settings = st.settings.get();
        settings.hide_host_calendar_when_locked = true;
        st.settings.set(settings);
        let f = app.global::<Frame>();
        f.set_agenda_current(st.models.agenda_current.clone().into());
        f.set_agenda_past(st.models.agenda_past.clone().into());
        f.set_tomorrow(st.models.tomorrow.clone().into());
        f.set_calendar_days(st.models.calendar_days.clone().into());
        wait_update(&st.calendar);
        refresh_calendar(&app, &st);
        let assert_hidden = || {
            assert!(f.get_host_calendar_hidden());
            assert_eq!(f.get_agenda_current().row_count(), 1);
            assert_eq!(
                f.get_agenda_current().row_data(0).unwrap().title,
                "Direct calendar"
            );
            assert_eq!(f.get_agenda_past().row_count(), 0);
            assert_eq!(f.get_calendar_event_count(), 1);
            assert_eq!(f.get_tomorrow().row_count(), 0);
            assert!(!f.get_next_up().contains("SECRET"));
            assert!(!f.get_agenda_done_sub().contains("SECRET"));
            assert!(f
                .get_calendar_days()
                .iter()
                .flat_map(|day| day.events.iter().collect::<Vec<_>>())
                .all(|event| !event.title.contains("SECRET") && !event.meta.contains("SECRET")));
        };
        assert_hidden();
        status_packet(&status_path, LockState::Unlocked);
        wait_update(&st.calendar);
        refresh_calendar(&app, &st);
        assert!(!f.get_host_calendar_hidden());
        assert_eq!(f.get_calendar_event_count(), 3);
        assert!(f.get_next_up().contains("SECRET"));
        assert_eq!(f.get_tomorrow().row_count(), 1);
        status_packet(&status_path, LockState::Locked);
        wait_update(&st.calendar);
        refresh_calendar(&app, &st);
        assert_hidden();
        // Calendar data remains intact while hidden and can reappear without another fetch.
        assert_eq!(
            Snapshot::load(&host_path).unwrap().sources[0].events.len(),
            3
        );
        status_packet(&status_path, LockState::Unlocked);
        wait_update(&st.calendar);
        refresh_calendar(&app, &st);
        assert_eq!(f.get_calendar_event_count(), 3);
        let restarted = Calendar::from_paths(host_path.clone(), Some(direct_path.clone())).unwrap();
        restarted.set_hide_when_locked(true);
        wait_update(&restarted);
        assert!(restarted.host_hidden());
        assert_eq!(
            restarted
                .events_on(now.with_timezone(&settings::zone()).date_naive())
                .len(),
            1
        );
        std::fs::write(&status_path, "broken").unwrap();
        wait_update(&st.calendar);
        refresh_calendar(&app, &st);
        assert_hidden();
        settings.hide_host_calendar_when_locked = false;
        st.settings.set(settings);
        refresh_calendar(&app, &st);
        assert_eq!(f.get_calendar_event_count(), 3);
        assert!(!f.get_host_calendar_hidden());
    })
    .join()
    .unwrap();
}

fn test_state(calendar: Calendar) -> State {
    State {
        settings: Cell::new(Settings {
            clock24: true,
            ..Settings::default()
        }),
        theme_transition: Timer::default(),
        chores: RefCell::new(vec![]),
        list: RefCell::new(vec![]),
        calendar,
        models: Models {
            agenda_past: Rc::new(VecModel::default()),
            agenda_current: Rc::new(VecModel::default()),
            tomorrow: Rc::new(VecModel::default()),
            calendar_days: Rc::new(VecModel::default()),
            list: Rc::new(VecModel::default()),
            chores: Rc::new(VecModel::default()),
        },
        screensaver: RefCell::new(None),
    }
}

fn in_clock_test_child(name: &str, preview: Option<&str>) -> bool {
    if std::env::var("FRAME_CALENDAR_CLOCK_TEST_CHILD")
        .ok()
        .as_deref()
        == Some(name)
    {
        return true;
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            &format!("calendar_ui_tests::{name}"),
            "--nocapture",
        ])
        .env("FRAME_CALENDAR_CLOCK_TEST_CHILD", name)
        .env_remove("FRAME_NOW");
    if let Some(preview) = preview {
        command.env("FRAME_NOW", preview);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

#[test]
fn home_and_calendar_share_one_instant_during_dst_fall_back() {
    if !in_clock_test_child(
        "home_and_calendar_share_one_instant_during_dst_fall_back",
        None,
    ) {
        return;
    }
    settings::set_zone_index(1);
    std::thread::spawn(|| {
        slint::platform::set_platform(Box::new(Headless)).unwrap();
        let app = AppWindow::new().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("host.json");
        Snapshot::new(vec![Source {
            id: "fixture".into(),
            events: [
                ("Ended", "2026-11-01T07:30:00Z", "2026-11-01T08:30:00Z"),
                ("Future", "2026-11-01T09:15:00Z", "2026-11-01T10:15:00Z"),
            ]
            .into_iter()
            .map(|(title, start, end)| WireEvent {
                id: title.into(),
                uid: None,
                title: title.into(),
                detail: String::new(),
                time: EventTime::Timed {
                    start: start.parse().unwrap(),
                    end: end.parse().unwrap(),
                },
            })
            .collect(),
        }])
        .save(&path)
        .unwrap();
        let state = test_state(Calendar::from_paths(path, None).unwrap());
        let frame = app.global::<Frame>();
        frame.set_agenda_current(state.models.agenda_current.clone().into());
        frame.set_agenda_past(state.models.agenda_past.clone().into());
        frame.set_calendar_days(state.models.calendar_days.clone().into());
        wait_update(&state.calendar);
        for (now, label) in [
            ("2026-11-01T08:45:00Z", "Future in 31 min"),
            ("2026-11-01T09:00:00Z", "Future in 16 min"),
            ("2026-11-01T09:15:00Z", "Future — happening now"),
        ] {
            refresh_calendar_at(&app, &state, now.parse().unwrap());
            assert_eq!(frame.get_agenda_past().row_count(), 1);
            assert_eq!(frame.get_agenda_past().row_data(0).unwrap().title, "Ended");
            assert_eq!(frame.get_agenda_current().row_count(), 1);
            assert_eq!(
                frame.get_agenda_current().row_data(0).unwrap().title,
                "Future"
            );
            assert_eq!(frame.get_next_up(), label);
            assert_eq!(frame.get_calendar_event_count(), 1);
            assert_eq!(
                frame
                    .get_calendar_days()
                    .row_data(0)
                    .unwrap()
                    .events
                    .row_data(0)
                    .unwrap()
                    .title,
                "Future"
            );
        }
        refresh_calendar_at(&app, &state, "2026-11-01T10:15:00Z".parse().unwrap());
        assert_eq!(frame.get_agenda_past().row_count(), 2);
        assert_eq!(frame.get_agenda_current().row_count(), 0);
        assert_eq!(frame.get_calendar_event_count(), 0);
        assert_eq!(frame.get_next_up(), "Nothing else planned today");
    })
    .join()
    .unwrap();
}

#[test]
fn frame_now_preview_drives_both_calendar_surfaces() {
    if !in_clock_test_child(
        "frame_now_preview_drives_both_calendar_surfaces",
        Some("12:34"),
    ) {
        return;
    }
    settings::set_zone_index(1);
    std::thread::spawn(|| {
        slint::platform::set_platform(Box::new(Headless)).unwrap();
        let app = AppWindow::new().unwrap();
        let instant = calendar_now();
        assert_eq!(
            instant
                .with_timezone(&settings::zone())
                .format("%H:%M")
                .to_string(),
            "12:34"
        );
        assert_eq!(now().format("%H:%M").to_string(), "12:34");
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("preview.json");
        let start = instant + CDur::minutes(30) + CDur::seconds(30);
        Snapshot::new(vec![Source {
            id: "fixture".into(),
            events: vec![WireEvent {
                id: "preview".into(),
                uid: None,
                title: "Preview".into(),
                detail: String::new(),
                time: EventTime::Timed {
                    start,
                    end: start + CDur::hours(1),
                },
            }],
        }])
        .save(&path)
        .unwrap();
        let state = test_state(Calendar::from_paths(path, None).unwrap());
        let frame = app.global::<Frame>();
        frame.set_agenda_current(state.models.agenda_current.clone().into());
        frame.set_calendar_days(state.models.calendar_days.clone().into());
        wait_update(&state.calendar);
        refresh_calendar(&app, &state);
        assert_eq!(frame.get_agenda_current().row_count(), 1);
        assert_eq!(frame.get_next_up(), "Preview in 31 min");
        assert_eq!(frame.get_calendar_event_count(), 1);
        assert_eq!(
            frame
                .get_calendar_days()
                .row_data(0)
                .unwrap()
                .events
                .row_data(0)
                .unwrap()
                .title,
            "Preview"
        );
    })
    .join()
    .unwrap();
}

#[test]
fn real_snapshots_drive_agenda_next_up_tomorrow_and_empty_states() {
    std::thread::spawn(|| {
        slint::platform::set_platform(Box::new(Headless)).unwrap();
        let app = AppWindow::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let host_path = dir.path().join("host.json");
        let direct_path = dir.path().join("direct.json");
        let start = Utc::now() - CDur::minutes(5);
        let end = Utc::now() + CDur::minutes(30);
        let today = now().date();
        let tomorrow = today + CDur::days(1);
        let meeting = WireEvent {
            id: "google-id".into(),
            uid: Some("shared-uid".into()),
            title: "Google review".into(),
            detail: "Office".into(),
            time: EventTime::Timed { start, end },
        };
        Snapshot::new(vec![Source {
            id: "google".into(),
            events: vec![meeting.clone()],
        }])
        .save(&direct_path)
        .unwrap();
        let next = settings::zone()
            .from_local_datetime(&tomorrow.and_hms_opt(10, 0, 0).unwrap())
            .single()
            .unwrap()
            .with_timezone(&Utc);
        Snapshot::new(vec![Source {
            id: "work".into(),
            events: vec![
                WireEvent {
                    id: "work-id".into(),
                    title: "Duplicate work review".into(),
                    ..meeting.clone()
                },
                WireEvent {
                    id: "all-day".into(),
                    uid: None,
                    title: "School holiday".into(),
                    detail: String::new(),
                    time: EventTime::AllDay {
                        start: today,
                        end: tomorrow,
                    },
                },
                WireEvent {
                    id: "tomorrow".into(),
                    uid: None,
                    title: "Tomorrow's meeting".into(),
                    detail: String::new(),
                    time: EventTime::Timed {
                        start: next,
                        end: next + CDur::hours(1),
                    },
                },
            ]
            .into_iter()
            .chain((0..3).map(|i| WireEvent {
                id: format!("tomorrow-all-day-{i}"),
                uid: None,
                title: format!("All-day notice {i}"),
                detail: String::new(),
                time: EventTime::AllDay {
                    start: tomorrow,
                    end: tomorrow + CDur::days(1),
                },
            }))
            .collect(),
        }])
        .save(&host_path)
        .unwrap();
        let st =
            test_state(Calendar::from_paths(host_path.clone(), Some(direct_path.clone())).unwrap());
        let f = app.global::<Frame>();
        f.set_agenda_current(st.models.agenda_current.clone().into());
        f.set_agenda_past(st.models.agenda_past.clone().into());
        f.set_tomorrow(st.models.tomorrow.clone().into());
        f.set_calendar_days(st.models.calendar_days.clone().into());
        refresh_calendar(&app, &st);
        assert_eq!(f.get_agenda_done(), "Waiting for calendar data.");
        assert!(!f.get_calendar_synced());
        assert_eq!(f.get_calendar_event_count(), 0);
        wait_update(&st.calendar);
        refresh_calendar(&app, &st);
        assert_eq!(f.get_agenda_current().row_count(), 2);
        assert_eq!(
            f.get_agenda_current().row_data(0).unwrap().title,
            "Google review"
        );
        assert_eq!(f.get_agenda_current().row_data(1).unwrap().time, "All day");
        assert_eq!(f.get_agenda_summary(), "2 events");
        assert!(f.get_next_up().starts_with("Google review"));
        assert!(f.get_calendar_synced());
        assert_eq!(f.get_calendar_event_count(), 6);
        assert_eq!(f.get_calendar_days().row_count(), 2);
        assert_eq!(f.get_calendar_days().row_data(0).unwrap().heading, "Today");
        assert_eq!(
            f.get_calendar_days().row_data(1).unwrap().heading,
            "Tomorrow"
        );
        assert_eq!(
            f.get_calendar_days()
                .row_data(1)
                .unwrap()
                .events
                .row_count(),
            4
        );
        assert!(f.get_calendar_updated().starts_with("Last synced"));
        let stable = f.get_calendar_days().row_data(0).unwrap().events;
        refresh_calendar(&app, &st);
        assert_eq!(f.get_calendar_days().row_data(0).unwrap().events, stable);
        assert_eq!(f.get_tomorrow().row_count(), 1);
        assert_eq!(f.get_tomorrow_count(), "1 event");
        assert_eq!(f.get_tomorrow().row_data(0).unwrap().time, "10:00");
        assert_eq!(
            f.get_tomorrow().row_data(0).unwrap().title,
            "Tomorrow's meeting"
        );
        Snapshot::new(vec![Source {
            id: "work".into(),
            events: (0..3)
                .map(|i| WireEvent {
                    id: format!("all-day-only-{i}"),
                    uid: None,
                    title: format!("Notice {i}"),
                    detail: String::new(),
                    time: EventTime::AllDay {
                        start: tomorrow,
                        end: tomorrow + CDur::days(1),
                    },
                })
                .collect(),
        }])
        .save(&host_path)
        .unwrap();
        wait_update(&st.calendar);
        refresh_calendar(&app, &st);
        assert_eq!(f.get_tomorrow().row_count(), 0);
        assert_eq!(f.get_tomorrow_count(), "0 events");
        assert_eq!(
            f.get_calendar_days()
                .row_data(1)
                .unwrap()
                .events
                .row_count(),
            3
        );
        assert_eq!(f.get_calendar_event_count(), 4);
        Snapshot::new(vec![]).save(&host_path).unwrap();
        wait_update(&st.calendar);
        refresh_calendar(&app, &st);
        assert_eq!(f.get_agenda_current().row_count(), 1);
        assert_eq!(f.get_agenda_summary(), "1 event");
        Snapshot::new(vec![]).save(&direct_path).unwrap();
        wait_update(&st.calendar);
        refresh_calendar(&app, &st);
        assert_eq!(f.get_agenda_current().row_count(), 0);
        assert_eq!(f.get_agenda_done(), "Nothing on the calendar today.");
        assert_eq!(f.get_next_up(), "Nothing else planned today");
        assert!(f.get_calendar_synced());
        assert_eq!(f.get_calendar_event_count(), 0);
    })
    .join()
    .unwrap();
}
