use super::*;
use host_state::LockState;

#[test]
fn lock_changes_send_before_calendar_interval_and_failures_are_retried() {
    let now = Instant::now();
    let mut reporter = LockReporter::default();
    let mut sent = Vec::new();
    reporter.poll(now, LockState::Unlocked, |status| {
        sent.push(status.state);
        Ok(())
    });
    reporter.poll(now + Duration::from_secs(1), LockState::Unlocked, |_| {
        panic!("sent too early")
    });
    reporter.poll(now + Duration::from_secs(2), LockState::Locked, |status| {
        sent.push(status.state);
        anyhow::bail!("disconnected")
    });
    reporter.poll(now + Duration::from_secs(7), LockState::Locked, |status| {
        sent.push(status.state);
        Ok(())
    });
    reporter.poll(
        now + Duration::from_secs(8),
        LockState::Unlocked,
        |status| {
            sent.push(status.state);
            Ok(())
        },
    );
    assert_eq!(
        sent,
        [
            LockState::Unlocked,
            LockState::Locked,
            LockState::Locked,
            LockState::Unlocked
        ]
    );
}

#[test]
fn stop_during_acquisition_does_not_publish_or_open_serial() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(
        &path,
        r#"{"connection":{"kind":"raw","port":"nonexistent-test-port"},"calendar":{"sources":[]}}"#,
    )
    .unwrap();
    let config = Config::load(&path).unwrap();
    let (control, commands) = mpsc::channel();
    let mut syncing = 0;
    watch_calendar(
        config,
        dir.path().to_owned(),
        commands,
        |event| match event {
            WatchEvent::Syncing => {
                syncing += 1;
                control.send(WatchControl::Stop).unwrap();
            }
            _ => panic!("stopped acquisition must not publish or open serial"),
        },
    )
    .unwrap();
    assert_eq!(syncing, 1);
}

#[test]
fn stopped_before_start_does_not_fetch() {
    let config: Config = serde_json::from_str(
        r#"{"connection":{"kind":"raw","port":"nonexistent-test-port"},"calendar":{"sources":[]}}"#,
    )
    .unwrap();
    let (control, commands) = mpsc::channel();
    control.send(WatchControl::Stop).unwrap();
    watch_calendar(config, PathBuf::from("."), commands, |_| {
        panic!("unexpected work")
    })
    .unwrap();
}

#[cfg(unix)]
#[test]
fn configured_json_source_roundtrips_through_receiver_and_replaces_deletions() {
    use calendar_sync::{Event, EventTime, Snapshot, Source};
    use std::io::{BufReader, Write};
    use std::os::unix::net::UnixStream;

    let dir = tempfile::tempdir().unwrap();
    let events_path = dir.path().join("events.json");
    let config_path = dir.path().join("config.json");
    let target = dir.path().join("frame.json");
    let event = Event {
        id: "work-1".into(),
        uid: Some("shared-1".into()),
        title: "Design review".into(),
        detail: "Office".into(),
        time: EventTime::Timed {
            start: "2026-11-02T18:00:00Z".parse().unwrap(),
            end: "2026-11-02T19:00:00Z".parse().unwrap(),
        },
    };
    std::fs::write(&events_path, serde_json::to_vec(&vec![&event]).unwrap()).unwrap();
    std::fs::write(
        &config_path,
        r#"{"calendar":{"sources":[{"kind":"json","id":"work","path":"events.json"}]}}"#,
    )
    .unwrap();
    let config = Config::load(&config_path).unwrap();
    let snapshot = config.calendar.fetch(dir.path(), Utc::now()).unwrap();
    let (mut host, frame) = UnixStream::pair().unwrap();
    // Match the transport's persistence window, not a shorter fixture-only deadline.
    host.set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let target_copy = target.clone();
    let receiver = std::thread::spawn(move || {
        calendar_sync::receive(
            BufReader::new(frame.try_clone().unwrap()),
            frame,
            &target_copy,
        )
        .unwrap();
    });
    serial::exchange(&mut host, &snapshot.encode().unwrap()).unwrap();
    receiver.join().unwrap();
    assert_eq!(Snapshot::load(&target).unwrap().sources[0].events[0], event);
    std::fs::write(&events_path, "[]").unwrap();
    let replacement = config.calendar.fetch(dir.path(), Utc::now()).unwrap();
    let mut wire = replacement.encode().unwrap();
    wire.push(b'\n');
    calendar_sync::receive(&wire[..], Vec::new(), &target).unwrap();
    assert!(Snapshot::load(&target).unwrap().sources[0]
        .events
        .is_empty());
    std::fs::write(&events_path, "not JSON").unwrap();
    assert!(config.calendar.fetch(dir.path(), Utc::now()).is_err());
    let direct = Snapshot::new(vec![Source {
        id: "google".into(),
        events: vec![event.clone()],
    }]);
    let host = Snapshot::new(vec![Source {
        id: "work".into(),
        events: vec![event],
    }]);
    assert_eq!(calendar_sync::merge([&direct, &host]).len(), 1);
    host.save(&target).unwrap();
    let before = std::fs::read(&target).unwrap();
    let mut output = Vec::new();
    assert!(calendar_sync::receive(&b"bad json\n"[..], &mut output, &target).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), before);
    output.flush().unwrap();
}
