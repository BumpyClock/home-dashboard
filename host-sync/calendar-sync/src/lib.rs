#[cfg(feature = "host")]
pub mod sources;

use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};

pub const VERSION: u32 = 1;
pub const MAX_BYTES: usize = 1024 * 1024;
pub const READY: &str = "FRAME_CALENDAR_READY_V1";
pub const STORED: &str = "FRAME_CALENDAR_STORED_V1";

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventTime {
    Timed {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
    AllDay {
        start: NaiveDate,
        end: NaiveDate,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    pub title: String,
    #[serde(default)]
    pub detail: String,
    pub time: EventTime,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub events: Vec<Event>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub generated_at: DateTime<Utc>,
    pub sources: Vec<Source>,
}

impl Snapshot {
    pub fn new(sources: Vec<Source>) -> Self {
        Self {
            version: VERSION,
            generated_at: Utc::now(),
            sources,
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.version == VERSION,
            "unsupported calendar version {}",
            self.version
        );
        ensure!(self.sources.len() <= 32, "too many calendar sources");
        let mut sources = HashSet::new();
        let mut count = 0;
        for source in &self.sources {
            ensure!(
                !source.id.trim().is_empty() && source.id.len() <= 128,
                "invalid source id"
            );
            ensure!(sources.insert(&source.id), "duplicate source id");
            let mut ids = HashSet::new();
            count += source.events.len();
            ensure!(count <= 5000, "too many events");
            for event in &source.events {
                ensure!(
                    !event.id.trim().is_empty() && event.id.len() <= 2048,
                    "invalid event id"
                );
                ensure!(
                    ids.insert(&event.id),
                    "duplicate event id in source {}",
                    source.id
                );
                ensure!(
                    !event.title.trim().is_empty() && event.title.len() <= 2048,
                    "invalid event title"
                );
                ensure!(event.detail.len() <= 8192, "event detail too long");
                if let Some(uid) = &event.uid {
                    ensure!(
                        !uid.trim().is_empty() && uid.len() <= 2048,
                        "invalid event uid"
                    );
                }
                let valid = match event.time {
                    EventTime::Timed { start, end } => end > start,
                    EventTime::AllDay { start, end } => end > start,
                };
                ensure!(valid, "event end must follow start");
            }
        }
        Ok(())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_BYTES, "calendar snapshot exceeds 1 MiB");
        let snapshot: Self = serde_json::from_slice(bytes).context("invalid calendar JSON")?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        ensure!(bytes.len() <= MAX_BYTES, "calendar snapshot exceeds 1 MiB");
        Ok(bytes)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        Self::decode(&bytes)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let bytes = self.encode()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        temp.persist(path)
            .context("could not replace calendar snapshot")?;
        Ok(())
    }
}

pub fn host_path() -> PathBuf {
    std::env::var_os("FRAME_CALENDAR_HOST")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
            PathBuf::from(home.unwrap_or_else(|| ".".into()))
                .join(".local/share/family-frame/calendar-host.json")
        })
}

#[derive(PartialEq, Eq, Hash)]
enum OccurrenceStart {
    Timed(DateTime<Utc>),
    AllDay(NaiveDate),
}

struct Occurrence {
    display: Event,
    precedence: usize,
    sources: HashSet<usize>,
}

// Index known identities and every alias first. Input order chooses display fields, not identity.
pub fn merge<'a>(snapshots: impl IntoIterator<Item = &'a Snapshot>) -> Vec<Event> {
    let records: Vec<_> = snapshots
        .into_iter()
        .flat_map(|s| &s.sources)
        .enumerate()
        .flat_map(|(source, calendar)| calendar.events.iter().map(move |event| (source, event)))
        .enumerate()
        .collect();
    let mut occurrences: Vec<Occurrence> = Vec::new();
    let mut by_uid = HashMap::new();
    let mut by_content: HashMap<(String, EventTime), Vec<usize>> = HashMap::new();
    for &(precedence, (source, event)) in records
        .iter()
        .filter(|(_, (_, e))| e.uid.is_some())
        .chain(records.iter().filter(|(_, (_, e))| e.uid.is_none()))
    {
        let content = (
            event
                .title
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase(),
            event.time.clone(),
        );
        let uid_key = event.uid.as_ref().map(|uid| {
            let start = match event.time {
                EventTime::Timed { start, .. } => OccurrenceStart::Timed(start),
                EventTime::AllDay { start, .. } => OccurrenceStart::AllDay(start),
            };
            (uid.clone(), start)
        });
        let duplicate = if let Some(key) = &uid_key {
            by_uid.get(key).copied()
        } else {
            by_content.get(&content).and_then(|candidates| {
                candidates
                    .iter()
                    .copied()
                    .filter(|&i| !occurrences[i].sources.contains(&source))
                    .min_by_key(|&i| occurrences[i].precedence)
            })
        };
        let index = if let Some(index) = duplicate {
            let occurrence = &mut occurrences[index];
            occurrence.sources.insert(source);
            if precedence < occurrence.precedence {
                let uid = occurrence.display.uid.clone();
                let mut display = event.clone();
                display.uid = display.uid.or(uid);
                occurrence.display = display;
                occurrence.precedence = precedence;
            }
            index
        } else {
            let index = occurrences.len();
            occurrences.push(Occurrence {
                display: event.clone(),
                precedence,
                sources: HashSet::from([source]),
            });
            index
        };
        if let Some(key) = uid_key {
            by_uid.insert(key, index);
        }
        let aliases = by_content.entry(content).or_default();
        if !aliases.contains(&index) {
            aliases.push(index);
        }
    }
    occurrences.sort_by_key(|occurrence| occurrence.precedence);
    occurrences
        .into_iter()
        .map(|occurrence| occurrence.display)
        .collect()
}

pub fn receive(input: impl BufRead, mut output: impl Write, path: &Path) -> Result<()> {
    writeln!(output, "{READY}")?;
    output.flush()?;
    let mut line = Vec::new();
    input
        .take((MAX_BYTES + 2) as u64)
        .read_until(b'\n', &mut line)?;
    let result = (|| {
        ensure!(line.last() == Some(&b'\n'), "incomplete calendar message");
        line.pop();
        let snapshot = Snapshot::decode(&line)?;
        snapshot.save(path)?;
        Ok::<_, anyhow::Error>(())
    })();
    if let Err(error) = result {
        writeln!(output, "FRAME_CALENDAR_ERROR_V1")?;
        output.flush()?;
        bail!(error);
    }
    writeln!(output, "{STORED}")?;
    output.flush()?;
    Ok(())
}
