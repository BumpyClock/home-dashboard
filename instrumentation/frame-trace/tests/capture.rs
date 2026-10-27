use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
fn child_capture() {
    let Ok(mode) = std::env::var("TRACE_TEST_MODE") else {
        return;
    };
    #[cfg(target_os = "linux")]
    if mode == "write_failure" {
        unsafe {
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
        }
    }
    let guard = frame_trace::init_from_env();
    #[cfg(target_os = "linux")]
    if mode == "write_failure" {
        for _ in 0..100 {
            if !frame_trace::enabled() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!frame_trace::enabled());
        return;
    }
    if mode == "failure" {
        assert!(!frame_trace::enabled());
        return;
    }
    assert!(frame_trace::enabled());
    let frame = frame_trace::begin_frame().unwrap();
    assert_eq!(frame_trace::current_frame(), Some(frame));
    assert_eq!(
        std::thread::spawn(frame_trace::current_frame)
            .join()
            .unwrap(),
        None
    );
    frame_trace::end_frame();
    assert_eq!(frame_trace::current_frame(), None);
    if mode == "duration" {
        std::thread::sleep(Duration::from_millis(1300));
        assert!(!frame_trace::enabled());
    } else {
        for i in 0..100_000 {
            frame_trace::event(
                "test_event",
                Some(i),
                &[frame_trace::Field::U64("value", i)],
            );
        }
    }
    drop(guard);
}

#[test]
fn capture_is_bounded_flushes_and_expires() {
    let dir = std::env::temp_dir().join(format!(
        "frame-trace-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let mut modes = vec!["capture", "duration", "failure"];
    if cfg!(target_os = "linux") {
        modes.push("write_failure");
    }
    for mode in modes {
        let path = dir.join(mode);
        if mode == "failure" {
            std::fs::write(&path, "existing").unwrap();
        }
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_capture", "--nocapture"])
            .env("TRACE_TEST_MODE", mode)
            .env("FRAME_TRACE_OUTPUT", &path)
            .env("FRAME_TRACE_SECONDS", "1")
            .status()
            .unwrap();
        assert!(status.success(), "{mode}");
        if mode == "failure" {
            assert_eq!(std::fs::read_to_string(path).unwrap(), "existing");
            continue;
        }
        let data = std::fs::read_to_string(path).unwrap();
        if mode == "write_failure" {
            assert!(data.is_empty());
            continue;
        }
        let lines: Vec<serde_json::Value> = data
            .lines()
            .map(|s| serde_json::from_str(s).unwrap())
            .collect();
        assert_eq!(lines[0]["type"], "session");
        assert_eq!(
            lines[0]["render_hooks_compiled"],
            cfg!(feature = "render-hooks")
        );
        assert!(lines[0]["gpu_completion_available"].is_null());
        assert_eq!(lines[0]["gpu_completion_timestamp_available"], false);
        let summary = lines.last().unwrap();
        assert_eq!(summary["type"], "summary");
        if mode == "capture" {
            let events: Vec<_> = lines.iter().filter(|v| v["type"] == "event").collect();
            let dropped = summary["dropped"].as_u64().unwrap();
            assert_eq!(events.len() as u64 + dropped, 100_000);
            assert!(dropped > 0);
            assert_eq!(summary["counts"]["test_event"], events.len());
            assert!(events.iter().all(|v| v["op_id"] == v["fields"]["value"]));
            assert!(events
                .windows(2)
                .all(|pair| pair[0]["at_us"].as_u64() <= pair[1]["at_us"].as_u64()));
        } else {
            assert!(summary["duration_us"].as_u64().unwrap() >= 1_000_000);
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
