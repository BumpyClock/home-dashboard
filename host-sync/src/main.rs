mod serial;
mod session;

use anyhow::{ensure, Context, Result};
use calendar_sync::sources;
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Parser)]
#[command(version, about = "Sync this computer's data to Family Frame")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List serial ports. Close any console/deploy tools before syncing.
    Ports,
    /// Print the current machine's lock state without sending data.
    HostState,
    /// Configure and sync calendar sources. Other host capabilities can be added alongside this.
    Calendar {
        #[command(subcommand)]
        command: CalendarCommand,
    },
}

#[derive(Subcommand)]
enum CalendarCommand {
    /// Fetch and send one complete calendar replacement.
    Sync {
        #[arg(long)]
        config: PathBuf,
    },
    /// Keep syncing. Failed cycles retain the last successful frame snapshot.
    Watch {
        #[arg(long)]
        config: PathBuf,
    },
    /// Fetch and write wire JSON to stdout. Does not open a serial port.
    Preview {
        #[arg(long)]
        config: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    connection: Option<serial::Connection>,
    #[serde(default = "default_interval")]
    interval_seconds: u64,
    calendar: sources::Config,
}

fn default_interval() -> u64 {
    300
}

impl Config {
    fn load(path: &Path) -> Result<Self> {
        let config: Self = serde_json::from_slice(&std::fs::read(path)?)
            .context("invalid host-sync configuration")?;
        ensure!(
            (30..=86400).contains(&config.interval_seconds),
            "interval_seconds must be 30..86400"
        );
        config.calendar.validate()?;
        if let Some(connection) = &config.connection {
            connection.validate()?;
        }
        Ok(config)
    }
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::HostState => println!("{}", serde_json::to_string(&session::current())?),
        Command::Ports => {
            for port in serialport::available_ports()? {
                println!("{}\t{:?}", port.port_name, port.port_type);
            }
        }
        Command::Calendar { command } => {
            let (path, watch, preview) = match command {
                CalendarCommand::Sync { config } => (config, false, false),
                CalendarCommand::Watch { config } => (config, true, false),
                CalendarCommand::Preview { config } => (config, false, true),
            };
            let config = Config::load(&path)?;
            if watch {
                return watch_calendar(config, path.parent().unwrap_or(Path::new(".")).to_owned());
            }
            let connection = if preview {
                None
            } else {
                Some(
                    config
                        .connection
                        .as_ref()
                        .context("sync requires a connection in config")?,
                )
            };
            let base = path.parent().unwrap_or(Path::new("."));
            let snapshot = config.calendar.fetch(base, Utc::now())?;
            if let Some(connection) = connection {
                connection.send(&snapshot)?;
                let count: usize = snapshot.sources.iter().map(|s| s.events.len()).sum();
                eprintln!(
                    "{} Calendar stored on frame. {count} source events.",
                    Utc::now().to_rfc3339()
                );
            } else {
                let stdout = std::io::stdout();
                let mut output = stdout.lock();
                output.write_all(&snapshot.encode()?)?;
                writeln!(output)?;
            }
        }
    }
    Ok(())
}

fn watch_calendar(mut config: Config, base: PathBuf) -> Result<()> {
    use std::sync::mpsc;
    use std::time::Instant;
    let connection = config
        .connection
        .take()
        .context("watch requires a connection")?;
    let interval = Duration::from_secs(config.interval_seconds);
    let monitor_lock = matches!(connection, serial::Connection::Console { .. });
    if !monitor_lock {
        eprintln!("Raw calendar transport has no lock heartbeat. A privacy-enabled frame will keep host events hidden.");
    }
    let (request, work) = mpsc::sync_channel(1);
    let (done, results) = mpsc::sync_channel(1);
    // Calendar calls can take 120 seconds. Only this loop owns serial, so lock updates never wait for them.
    std::thread::spawn(move || {
        while work.recv().is_ok() {
            let result = config.calendar.fetch(&base, Utc::now());
            if done.send(result).is_err() {
                break;
            }
        }
    });
    let mut reporter = LockReporter::default();
    let mut next_fetch = Instant::now();
    let mut busy = false;
    loop {
        if monitor_lock {
            reporter.poll(Instant::now(), session::current(), |status| {
                connection.send_status(status)
            });
        }
        if !busy && Instant::now() >= next_fetch {
            request.send(()).context("calendar worker stopped")?;
            busy = true;
        }
        match results.try_recv() {
            Ok(result) => {
                let result = result.and_then(|snapshot| {
                    connection.send(&snapshot)?;
                    eprintln!(
                        "{} Calendar stored on frame. {} source events.",
                        Utc::now().to_rfc3339(),
                        snapshot
                            .sources
                            .iter()
                            .map(|s| s.events.len())
                            .sum::<usize>()
                    );
                    Ok(())
                });
                if let Err(error) = result {
                    eprintln!("Calendar sync failed. Previous frame data retained. {error:#}");
                }
                busy = false;
                next_fetch = Instant::now() + interval;
                if monitor_lock {
                    reporter.poll(Instant::now(), session::current(), |status| {
                        connection.send_status(status)
                    });
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => anyhow::bail!("calendar worker stopped"),
            Err(mpsc::TryRecvError::Empty) => {}
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[derive(Default)]
struct LockReporter {
    state: Option<host_state::LockState>,
    last_attempt: Option<std::time::Instant>,
}
impl LockReporter {
    fn poll(
        &mut self,
        now: std::time::Instant,
        state: host_state::LockState,
        send: impl FnOnce(&host_state::Status) -> Result<()>,
    ) {
        let due = self.state != Some(state)
            || self
                .last_attempt
                .is_none_or(|last| now.saturating_duration_since(last) >= host_state::HEARTBEAT);
        if !due {
            return;
        }
        self.last_attempt = Some(now);
        self.state = Some(state);
        if let Err(error) = send(&host_state::Status::new(state)) {
            eprintln!("Host lock heartbeat failed; privacy-enabled frame will hide host events. {error:#}");
        }
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    use host_state::LockState;
    #[test]
    fn lock_changes_send_before_calendar_interval_and_failures_are_retried() {
        let now = std::time::Instant::now();
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
}

#[cfg(all(test, unix))]
mod integration_tests {
    use super::*;
    use calendar_sync::{Event, EventTime, Snapshot, Source};
    use std::io::{BufReader, Write};
    use std::os::unix::net::UnixStream;

    #[test]
    fn configured_json_source_roundtrips_through_receiver_and_replaces_deletions() {
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
        host.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
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
        // A source failure cannot emit a partial replacement.
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
}
