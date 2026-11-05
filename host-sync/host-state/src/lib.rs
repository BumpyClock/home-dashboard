use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const READY: &str = "FRAME_HOST_STATE_READY_V1";
pub const STORED: &str = "FRAME_HOST_STATE_STORED_V1";
pub const ERROR: &str = "FRAME_HOST_STATE_ERROR_V1";
pub const LEASE: Duration = Duration::from_secs(20);
pub const HEARTBEAT: Duration = Duration::from_secs(5);
const MAX_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LockState {
    Locked,
    Unlocked,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    version: u32,
    pub state: LockState,
    checked_at: DateTime<Utc>,
}

impl Status {
    pub fn new(state: LockState) -> Self {
        Self {
            version: 1,
            state,
            checked_at: Utc::now(),
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
    fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_BYTES, "host status exceeds 1 KiB");
        let status: Self = serde_json::from_slice(bytes).context("invalid host status")?;
        ensure!(status.version == 1, "unsupported host status version");
        Ok(status)
    }
    pub fn load(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        Self::decode(&bytes)
    }
}

pub fn path_for_calendar(calendar_path: &Path) -> PathBuf {
    calendar_path.with_extension("host-state.json")
}

// A fresh frame-side monotonic lease, never the host's clock, controls visibility.
#[derive(Default)]
pub struct Visibility {
    last: Option<(LockState, Instant)>,
}
impl Visibility {
    pub fn observe(&mut self, state: LockState, received: Instant) {
        self.last = Some((state, received));
    }
    pub fn state_at(&self, now: Instant) -> LockState {
        self.last
            .filter(|(_, received)| now.saturating_duration_since(*received) < LEASE)
            .map_or(LockState::Unknown, |(state, _)| state)
    }
    pub fn allows_host(&self, now: Instant) -> bool {
        self.state_at(now) == LockState::Unlocked
    }
}

pub fn receive(input: impl BufRead, mut output: impl Write, path: &Path) -> Result<()> {
    writeln!(output, "{READY}")?;
    output.flush()?;
    let result = (|| {
        let mut line = Vec::new();
        input
            .take((MAX_BYTES + 2) as u64)
            .read_until(b'\n', &mut line)?;
        ensure!(line.last() == Some(&b'\n'), "incomplete host status");
        line.pop();
        let status = Status::decode(&line)?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(&status.encode()?)?;
        temp.persist(path).context("could not store host status")?;
        Ok::<_, anyhow::Error>(())
    })();
    writeln!(output, "{}", if result.is_ok() { STORED } else { ERROR })?;
    output.flush()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_lease_transition_contract() {
        use LockState::{Locked, Unknown, Unlocked};

        let start = Instant::now();
        let fresh = Duration::ZERO;
        let renewal = LEASE - Duration::from_secs(1);
        let tick = Duration::from_nanos(1);
        let expiry = renewal + LEASE;
        let resumed = expiry + tick;
        let locked_expiry = resumed + LEASE;
        let mut privacy = Visibility::default();
        for (case, observation, elapsed, expected) in [
            ("startup", None, fresh, Unknown),
            ("unlock", Some((Unlocked, fresh)), fresh, Unlocked),
            (
                "heartbeat extends original lease",
                Some((Unlocked, renewal)),
                LEASE,
                Unlocked,
            ),
            ("last fresh tick", None, expiry - tick, Unlocked),
            ("exact expiry", None, expiry, Unknown),
            (
                "queued old unlock cannot renew",
                Some((Unlocked, renewal)),
                resumed,
                Unknown,
            ),
            (
                "fresh recovery",
                Some((Unlocked, resumed)),
                resumed,
                Unlocked,
            ),
            ("lock revokes", Some((Locked, resumed)), resumed, Locked),
            ("lock expires to unknown", None, locked_expiry, Unknown),
            (
                "unlock after lock expiry",
                Some((Unlocked, locked_expiry)),
                locked_expiry,
                Unlocked,
            ),
            (
                "unknown revokes",
                Some((Unknown, locked_expiry)),
                locked_expiry,
                Unknown,
            ),
        ] {
            if let Some((state, received)) = observation {
                privacy.observe(state, start + received);
            }
            assert_eq!(privacy.state_at(start + elapsed), expected, "{case}");
            assert_eq!(
                privacy.allows_host(start + elapsed),
                expected == LockState::Unlocked,
                "{case}"
            );
        }
    }

    #[test]
    fn receiver_stores_status_only_and_rejects_malformed_messages() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("status.json");
        let status = Status::new(LockState::Locked);
        let mut wire = status.encode().unwrap();
        wire.push(b'\n');
        let mut output = Vec::new();
        receive(&wire[..], &mut output, &path).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            format!("{READY}\n{STORED}\n")
        );
        assert_eq!(Status::load(&path).unwrap(), status);
        for wire in [
            b"{}\n".to_vec(),
            b"{\"version\":2,\"state\":\"unlocked\",\"checked_at\":\"2026-11-01T10:00:00Z\"}\n"
                .to_vec(),
            vec![b'x'; MAX_BYTES + 2],
        ] {
            assert!(receive(&wire[..], Vec::new(), &path).is_err());
            assert_eq!(Status::load(&path).unwrap(), status);
        }
        assert!(!Visibility::default().allows_host(Instant::now()));
    }
}
