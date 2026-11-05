pub mod serial;
pub mod session;

#[cfg(test)]
mod tests;

use anyhow::{ensure, Context, Result};
use calendar_sync::sources;
use chrono::Utc;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub connection: Option<serial::Connection>,
    #[serde(default = "default_interval")]
    pub interval_seconds: u64,
    pub calendar: sources::Config,
}

fn default_interval() -> u64 {
    300
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        Self::parse(&std::fs::read(path)?)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let config: Self =
            serde_json::from_slice(bytes).context("invalid host-sync configuration")?;
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

pub enum WatchControl {
    SyncNow,
    Stop,
}

pub enum WatchEvent {
    Syncing,
    Synced { at: String, events: usize },
    CalendarFailed(String),
    Privacy { error: Option<String> },
}

/// One serial owner for both CLI and desktop. Stop waits for in-flight provider cleanup,
/// but never publishes its calendar result after observing the stop request.
pub fn watch_calendar(
    mut config: Config,
    base: PathBuf,
    control: mpsc::Receiver<WatchControl>,
    mut observe: impl FnMut(WatchEvent),
) -> Result<()> {
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
    // Only this loop owns serial. Slow providers cannot block lock heartbeats.
    let worker = std::thread::spawn(move || {
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
    let mut sync_again = false;
    let result = (|| loop {
        for command in control.try_iter() {
            match command {
                WatchControl::Stop => return Ok(()),
                WatchControl::SyncNow => sync_again = true,
            }
        }
        if !busy && (sync_again || Instant::now() >= next_fetch) {
            request.send(()).context("calendar worker stopped")?;
            busy = true;
            sync_again = false;
            observe(WatchEvent::Syncing);
        }
        if monitor_lock {
            reporter.poll(Instant::now(), session::current(), |status| {
                let result = connection.send_status(status);
                observe(WatchEvent::Privacy {
                    error: result.as_ref().err().map(|error| format!("{error:#}")),
                });
                result
            });
        }
        match results.recv_timeout(Duration::from_secs(2)) {
            Ok(result) => {
                for command in control.try_iter() {
                    match command {
                        WatchControl::Stop => return Ok(()),
                        WatchControl::SyncNow => sync_again = true,
                    }
                }
                let result = result.and_then(|snapshot| {
                    connection.send(&snapshot)?;
                    let at = Utc::now().to_rfc3339();
                    let events = snapshot.sources.iter().map(|s| s.events.len()).sum();
                    eprintln!("{at} Calendar stored on frame. {events} source events.");
                    observe(WatchEvent::Synced { at, events });
                    Ok(())
                });
                if let Err(error) = result {
                    eprintln!("Calendar sync failed. Previous frame data retained. {error:#}");
                    observe(WatchEvent::CalendarFailed(format!("{error:#}")));
                }
                busy = false;
                next_fetch = Instant::now() + interval;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("calendar worker stopped")
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    })();
    drop(request);
    drop(results);
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("calendar worker panicked"))?;
    result
}

#[derive(Default)]
struct LockReporter {
    state: Option<host_state::LockState>,
    last_attempt: Option<Instant>,
}

impl LockReporter {
    fn poll(
        &mut self,
        now: Instant,
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
