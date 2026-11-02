use crate::{Event, EventTime, Snapshot, Source};
use anyhow::{bail, ensure, Context, Result};
use chrono::{DateTime, Duration, Local, NaiveDateTime, TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration as StdDuration;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_days")]
    pub days: u32,
    #[serde(default)]
    pub time_zone: Option<chrono_tz::Tz>,
    pub sources: Vec<SourceConfig>,
}

fn default_days() -> u32 {
    7
}
fn default_mcporter() -> String {
    "mcporter".into()
}
fn default_calendar_server() -> String {
    "MicrosoftCalendar".into()
}
fn default_workiq_server() -> String {
    "Workiq".into()
}
fn default_workiq() -> String {
    "workiq".into()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceConfig {
    Workiq {
        id: String,
        #[serde(default = "default_workiq")]
        executable: String,
    },
    WorkiqMcp {
        id: String,
        #[serde(default = "default_mcporter")]
        executable: String,
        #[serde(default = "default_workiq_server")]
        server: String,
    },
    McporterCalendar {
        id: String,
        #[serde(default = "default_mcporter")]
        executable: String,
        #[serde(default = "default_calendar_server")]
        server: String,
    },
    Json {
        id: String,
        path: PathBuf,
    },
}

impl SourceConfig {
    pub fn id(&self) -> &str {
        match self {
            Self::Workiq { id, .. }
            | Self::WorkiqMcp { id, .. }
            | Self::McporterCalendar { id, .. }
            | Self::Json { id, .. } => id,
        }
    }
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        ensure!((1..=31).contains(&self.days), "calendar.days must be 1..31");
        ensure!(self.sources.len() <= 32, "too many sources");
        let mut ids = HashSet::new();
        for source in &self.sources {
            ensure!(
                !source.id().trim().is_empty() && source.id().len() <= 128,
                "invalid source id"
            );
            ensure!(ids.insert(source.id()), "duplicate source id");
        }
        Ok(())
    }

    // One complete replacement, never a partial success that deletes a failed source's events.
    pub fn fetch(&self, base: &Path, now: DateTime<Utc>) -> Result<Snapshot> {
        let (start, end) = match self.time_zone {
            Some(zone) => calendar_window(now, self.days, zone)?,
            None => calendar_window(now, self.days, Local)?,
        };
        let mut sources = Vec::new();
        for source in &self.sources {
            let events = (|| -> Result<Vec<Event>> {
                match source {
                    SourceConfig::Json { path, .. } => {
                        let file = std::fs::File::open(base.join(path))?;
                        let mut bytes = Vec::new();
                        file.take((crate::MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
                        ensure!(bytes.len() <= crate::MAX_BYTES, "JSON source exceeds 1 MiB");
                        Ok(serde_json::from_slice(&bytes)?)
                    }
                    SourceConfig::Workiq { executable, .. } => {
                        let mut command = Command::new(executable);
                        command.args(["ask", "-q", &question(start, end)]);
                        let text = run(command)?;
                        workiq_events(&text)
                    }
                    SourceConfig::WorkiqMcp { executable, server, .. } => {
                        let value = call(executable, &format!("{server}.ask"), json!({"question": question(start, end)}))?;
                        workiq_value(value)
                    }
                    SourceConfig::McporterCalendar { executable, server, .. } => {
                        let value = call(executable, &format!("{server}.ListCalendarView"), json!({
                            "startDateTime": start.to_rfc3339(), "endDateTime": end.to_rfc3339(),
                            "timeZone": "UTC", "top": 1000, "orderby": "start/dateTime",
                            "select": "id,iCalUId,subject,start,end,isAllDay,isCancelled,location"
                        }))?;
                        graph_events(value)
                    }
                }
            })().with_context(|| format!("calendar source {} failed", source.id()))?;
            sources.push(Source {
                id: source.id().into(),
                events,
            });
        }
        let snapshot = Snapshot::new(sources);
        snapshot.encode()?;
        Ok(snapshot)
    }
}

fn calendar_window(
    now: DateTime<Utc>,
    days: u32,
    zone: impl TimeZone,
) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
    let today = now.with_timezone(&zone).date_naive();
    let midnight = |day: chrono::NaiveDate| {
        zone.from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .map(|time| time.with_timezone(&Utc))
            .context("calendar day has no midnight in this time zone")
    };
    Ok((
        midnight(today)?,
        midnight(today + Duration::days(i64::from(days)))?,
    ))
}

fn question(start: DateTime<Utc>, end: DateTime<Utc>) -> String {
    format!("Read my calendar from {} inclusive to {} exclusive. Expand every recurring occurrence. \
        Exclude cancelled events. Return ONLY a JSON object with keys complete (boolean) and events (array). \
        Set complete=true only if every event in this window was retrieved, even if the array is empty. \
        If retrieval fails or is truncated set complete=false. No markdown or commentary. \
        Each event must have id (actual occurrence ID, not invented), uid (iCalendar UID if available, else null), \
        title (subject), detail (location only, empty if absent), and time. \
        Timed time is {{\"kind\":\"timed\",\"start\":\"RFC3339 with UTC offset\",\"end\":\"RFC3339 with UTC offset\"}}. \
        All-day time is {{\"kind\":\"all_day\",\"start\":\"YYYY-MM-DD\",\"end\":\"YYYY-MM-DD\"}} with exclusive end date. \
        Do not include descriptions, attendees, meeting URLs, or unrelated workplace data.", start.to_rfc3339(), end.to_rfc3339())
}

fn call(executable: &str, tool: &str, args: Value) -> Result<Value> {
    let mut command = Command::new(executable);
    command.args([
        "call",
        tool,
        "--args",
        &args.to_string(),
        "--output",
        "json",
        "--no-oauth",
        "--timeout",
        "110000",
    ]);
    let text = run(command)?;
    unwrap_mcp(serde_json::from_str(&text).context("mcporter returned non-JSON output")?)
}

fn run(command: Command) -> Result<String> {
    let bytes = host_command::output(command, StdDuration::from_secs(120), 4 * 1024 * 1024)
        .context("calendar provider failed; run CLI manually to inspect authentication/errors")?;
    String::from_utf8(bytes).context("provider output is not UTF-8")
}

fn unwrap_mcp(value: Value) -> Result<Value> {
    ensure!(
        value.get("isError").and_then(Value::as_bool) != Some(true) && value.get("error").is_none(),
        "MCP returned an error"
    );
    if let Some(structured) = value.get("structuredContent").filter(|v| !v.is_null()) {
        return Ok(structured.clone());
    }
    if let Some(content) = value.get("content").and_then(Value::as_array) {
        let mut payloads = content
            .iter()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .map(str::trim)
            .filter(|text| !text.is_empty() && !text.starts_with("CorrelationId:"));
        let text = payloads.next().context("MCP returned no payload text")?;
        ensure!(
            payloads.next().is_none(),
            "MCP returned multiple payloads; refusing an ambiguous calendar replacement"
        );
        return Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.into())));
    }
    Ok(value)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkiqResult {
    complete: bool,
    events: Vec<Event>,
}

fn workiq_events(text: &str) -> Result<Vec<Event>> {
    let text = text.trim();
    let text = text
        .strip_prefix("```json")
        .or_else(|| text.strip_prefix("```"))
        .and_then(|s| s.trim().strip_suffix("```"))
        .unwrap_or(text)
        .trim();
    let value: Value = serde_json::from_str(text)
        .context("WorkIQ must return complete structured JSON, not prose")?;
    workiq_value(value)
}

fn workiq_value(value: Value) -> Result<Vec<Event>> {
    match value {
        Value::String(text) => workiq_events(&text),
        Value::Object(ref object) if object.contains_key("complete") => {
            let result: WorkiqResult = serde_json::from_value(value)?;
            ensure!(
                result.complete,
                "WorkIQ did not confirm a complete calendar result; use mcporter_calendar for deterministic calendar export"
            );
            Ok(result.events)
        }
        Value::Object(ref object) => {
            for key in ["answer", "markdown", "text", "response"] {
                if let Some(text) = object.get(key).and_then(Value::as_str) {
                    return workiq_events(text);
                }
            }
            bail!("WorkIQ response has no structured calendar answer")
        }
        _ => bail!("WorkIQ response is not a calendar object"),
    }
}

#[derive(Deserialize)]
struct GraphTime {
    #[serde(rename = "dateTime")]
    datetime: String,
    #[serde(rename = "timeZone")]
    zone: Option<String>,
}
#[derive(Deserialize)]
struct GraphLocation {
    #[serde(default, rename = "displayName")]
    name: String,
}
#[derive(Deserialize)]
struct GraphEvent {
    id: String,
    #[serde(rename = "iCalUId", alias = "iCalUID")]
    uid: Option<String>,
    subject: String,
    start: GraphTime,
    end: GraphTime,
    #[serde(default, rename = "isAllDay")]
    all_day: bool,
    #[serde(default, rename = "isCancelled")]
    cancelled: bool,
    location: Option<GraphLocation>,
}

fn graph_time(time: GraphTime) -> Result<DateTime<Utc>> {
    if let Ok(time) = DateTime::parse_from_rfc3339(&time.datetime) {
        return Ok(time.with_timezone(&Utc));
    }
    let naive = NaiveDateTime::parse_from_str(&time.datetime, "%Y-%m-%dT%H:%M:%S%.f")?;
    match time.zone.as_deref() {
        Some("UTC" | "Etc/UTC") => Ok(Utc.from_utc_datetime(&naive)),
        _ => bail!("calendar returned a naive timestamp without UTC despite timeZone=UTC"),
    }
}

fn graph_events(value: Value) -> Result<Vec<Event>> {
    let value = if let Value::String(text) = value {
        let json = text
            .strip_prefix("Calendar view retrieved successfully.")
            .unwrap_or(&text)
            .trim();
        serde_json::from_str(json).context("Calendar MCP did not return valid Graph JSON")?
    } else {
        value
    };
    ensure!(
        value.get("@odata.nextLink").is_none_or(Value::is_null)
            && value.get("nextLink").is_none_or(Value::is_null)
            && value.get("hasMore").and_then(Value::as_bool) != Some(true),
        "calendar result is paginated; reduce calendar.days"
    );
    let rows = if value.is_array() {
        value
    } else {
        value
            .get("value")
            .or_else(|| value.get("events"))
            .context("calendar response missing events array")?
            .clone()
    };
    let rows: Vec<GraphEvent> = serde_json::from_value(rows)?;
    ensure!(
        rows.len() < 1000,
        "calendar result may be truncated at 1000 events; reduce calendar.days"
    );
    rows.into_iter()
        .filter(|e| !e.cancelled)
        .map(|event| {
            let time = if event.all_day {
                EventTime::AllDay {
                    start: event
                        .start
                        .datetime
                        .get(..10)
                        .context("invalid all-day start")?
                        .parse()?,
                    end: event
                        .end
                        .datetime
                        .get(..10)
                        .context("invalid all-day end")?
                        .parse()?,
                }
            } else {
                EventTime::Timed {
                    start: graph_time(event.start)?,
                    end: graph_time(event.end)?,
                }
            };
            Ok(Event {
                id: event.id,
                uid: event.uid,
                title: event.subject,
                detail: event.location.map_or_else(String::new, |l| l.name),
                time,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workiq_requires_complete_json_and_accepts_mcp_text() {
        let answer = json!({"answer": "```json\n{\"complete\":true,\"events\":[]}\n```"});
        assert!(workiq_value(
            unwrap_mcp(json!({"content":[{"type":"text","text":answer.to_string()}]})).unwrap()
        )
        .unwrap()
        .is_empty());
        assert!(workiq_events("{\"complete\":false,\"events\":[]}").is_err());
        assert!(workiq_events("No meetings found.").is_err());
        assert!(workiq_events("[]").is_err());
        assert!(unwrap_mcp(json!({"isError":true,"content":[]})).is_err());
    }

    #[test]
    fn graph_maps_offsets_all_day_and_cancellations() {
        let timed = json!({"id":"instance-1","iCalUId":"uid-1","subject":"Standup",
            "start":{"dateTime":"2026-11-02T10:00:00-08:00"},"end":{"dateTime":"2026-11-02T10:30:00-08:00"},
            "location":{"displayName":"Office"}});
        let all_day = json!({"id":"holiday","subject":"Holiday","isAllDay":true,
            "start":{"dateTime":"2026-11-03T00:00:00","timeZone":"UTC"},
            "end":{"dateTime":"2026-11-04T00:00:00","timeZone":"UTC"}});
        let mut cancelled = timed.clone();
        cancelled["isCancelled"] = json!(true);
        let events = graph_events(json!({"value":[timed, all_day, cancelled]})).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].time,
            EventTime::Timed {
                start: "2026-11-02T18:00:00Z".parse().unwrap(),
                end: "2026-11-02T18:30:00Z".parse().unwrap()
            }
        );
        assert_eq!(events[0].detail, "Office");
        assert_eq!(
            events[1].time,
            EventTime::AllDay {
                start: "2026-11-03".parse().unwrap(),
                end: "2026-11-04".parse().unwrap()
            }
        );
        assert!(graph_events(json!({"value":[],"@odata.nextLink":"more"})).is_err());
        assert!(graph_events(json!({"error":"unavailable"})).is_err());
    }

    #[test]
    fn naive_non_utc_is_not_silently_shifted() {
        assert!(graph_time(GraphTime {
            datetime: "2026-11-02T10:00:00".into(),
            zone: Some("Pacific Standard Time".into())
        })
        .is_err());
        assert_eq!(
            graph_time(GraphTime {
                datetime: "2026-11-02T10:00:00.0000000".into(),
                zone: Some("UTC".into())
            })
            .unwrap()
            .to_rfc3339(),
            "2026-11-02T10:00:00+00:00"
        );
    }
}
