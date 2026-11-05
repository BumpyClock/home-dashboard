use calendar_sync::{Event as CalendarEvent, EventTime, Snapshot};
use chrono::{DateTime, Duration as CalendarDuration, NaiveDate, NaiveDateTime, Utc};
use host_state::{LockState, Status, Visibility};
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct Event {
    pub title: String,
    pub detail: String,
    time: EventTime,
    zone: chrono_tz::Tz,
}

pub struct HomeAgenda {
    pub past: Vec<Event>,
    pub current: Vec<Event>,
    pub tomorrow: Vec<Event>,
}

pub struct UpcomingDay {
    pub date: NaiveDate,
    pub events: Vec<Event>,
}

pub struct Upcoming {
    pub days: Vec<UpcomingDay>,
    pub event_count: usize,
}

impl Event {
    pub fn local_start(&self) -> NaiveDateTime {
        match self.time {
            EventTime::Timed { start, .. } => start.with_timezone(&self.zone).naive_local(),
            EventTime::AllDay { start, .. } => start.and_hms_opt(0, 0, 0).unwrap(),
        }
    }

    pub fn local_end(&self) -> NaiveDateTime {
        match self.time {
            EventTime::Timed { end, .. } => end.with_timezone(&self.zone).naive_local(),
            EventTime::AllDay { end, .. } => end.and_hms_opt(0, 0, 0).unwrap(),
        }
    }

    pub fn is_all_day(&self) -> bool {
        matches!(self.time, EventTime::AllDay { .. })
    }

    pub fn pending_at(&self, now: DateTime<Utc>) -> bool {
        pending_at(&self.time, now, self.zone)
    }

    pub fn minutes_until(&self, now: DateTime<Utc>) -> Option<i64> {
        match self.time {
            EventTime::Timed { start, .. } if start > now => Some((start - now).num_minutes() + 1),
            _ => None,
        }
    }

    pub fn happening_at(&self, now: DateTime<Utc>) -> bool {
        match self.time {
            EventTime::Timed { start, end } => start <= now && end > now,
            EventTime::AllDay { start, end } => {
                let today = now.with_timezone(&self.zone).date_naive();
                start <= today && end > today
            }
        }
    }

    fn compare_start(&self, other: &Self) -> Ordering {
        match (&self.time, &other.time) {
            (EventTime::Timed { start: a, .. }, EventTime::Timed { start: b, .. }) => a.cmp(b),
            (EventTime::AllDay { start: a, .. }, EventTime::AllDay { start: b, .. }) => a.cmp(b),
            (EventTime::AllDay { .. }, _) => Ordering::Less,
            (_, EventTime::AllDay { .. }) => Ordering::Greater,
        }
    }
}

fn pending_at(time: &EventTime, now: DateTime<Utc>, zone: chrono_tz::Tz) -> bool {
    match time {
        EventTime::Timed { end, .. } => *end > now,
        EventTime::AllDay { end, .. } => *end > now.with_timezone(&zone).date_naive(),
    }
}

#[derive(Default)]
struct Contents {
    loaded: bool,
    events: Vec<CalendarEvent>,
    direct_events: Vec<CalendarEvent>,
    updated_at: Option<DateTime<Utc>>,
    direct_updated_at: Option<DateTime<Utc>>,
}

impl Contents {
    fn visible(&self, host_hidden: bool) -> &[CalendarEvent] {
        if host_hidden {
            &self.direct_events
        } else {
            &self.events
        }
    }
}

pub struct Calendar {
    contents: RefCell<Contents>,
    visibility: RefCell<Visibility>,
    hide_when_locked: Cell<bool>,
    hidden: Cell<bool>,
    privacy_state: Cell<LockState>,
    updates: Receiver<Contents>,
    host_updates: Arc<Mutex<Option<(LockState, Instant)>>>,
}

impl Calendar {
    pub fn from_env() -> Result<Self, Box<dyn std::error::Error>> {
        let direct = match std::env::var("FRAME_CALENDAR_MODE")
            .as_deref()
            .unwrap_or("combined")
        {
            "host" => None,
            "combined" => std::env::var_os("FRAME_CALENDAR_DIRECT").map(PathBuf::from),
            _ => return Err("FRAME_CALENDAR_MODE must be host or combined".into()),
        };
        Self::from_paths(calendar_sync::host_path(), direct)
    }

    pub fn from_paths(
        host: PathBuf,
        direct: Option<PathBuf>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (tx, updates) = mpsc::sync_channel(1);
        let host_updates = Arc::new(Mutex::new(None));
        let pending = Arc::downgrade(&host_updates);
        let status_path = host_state::path_for_calendar(&host);
        std::thread::Builder::new()
            .name("calendar-privacy".into())
            .spawn(move || {
                // A restart never turns a persisted unlocked packet into fresh permission.
                let mut previous_status = Status::load(&status_path).ok();
                loop {
                    if pending.strong_count() == 0 {
                        return;
                    }
                    let status = Status::load(&status_path).ok();
                    if status != previous_status {
                        let state = status
                            .as_ref()
                            .map_or(LockState::Unknown, |status| status.state);
                        previous_status = status;
                        let received = Instant::now();
                        let Some(pending) = pending.upgrade() else {
                            return;
                        };
                        *pending.lock().unwrap() = Some((state, received));
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
            })?;
        let lifetime = Arc::downgrade(&host_updates);
        std::thread::Builder::new()
            .name("calendar-input".into())
            .spawn(move || {
                let mut host_input = Input::new(host);
                let mut direct_input = direct.map(Input::new);
                loop {
                    if lifetime.strong_count() == 0 {
                        return;
                    }
                    let mut changed = host_input.refresh();
                    if let Some(input) = &mut direct_input {
                        changed |= input.refresh();
                    }
                    if changed {
                        let snapshots: Vec<_> = direct_input
                            .as_ref()
                            .and_then(|i| i.snapshot.as_ref())
                            .into_iter()
                            .chain(host_input.snapshot.as_ref())
                            .collect();
                        let direct = direct_input
                            .as_ref()
                            .and_then(|input| input.snapshot.as_ref());
                        let update = Contents {
                            loaded: !snapshots.is_empty(),
                            updated_at: snapshots.iter().map(|s| s.generated_at).max(),
                            events: calendar_sync::merge(snapshots),
                            direct_events: calendar_sync::merge(direct),
                            direct_updated_at: direct.map(|snapshot| snapshot.generated_at),
                        };
                        if tx.send(update).is_err() {
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_secs(2));
                }
            })?;
        Ok(Self {
            contents: RefCell::new(Contents::default()),
            visibility: RefCell::new(Visibility::default()),
            hide_when_locked: Cell::new(false),
            hidden: Cell::new(false),
            privacy_state: Cell::new(LockState::Unknown),
            updates,
            host_updates,
        })
    }

    pub fn poll(&self) -> bool {
        let mut changed = false;
        while let Ok(contents) = self.updates.try_recv() {
            *self.contents.borrow_mut() = contents;
            changed = true;
        }
        if let Some((state, received)) = self.host_updates.lock().unwrap().take() {
            self.visibility.borrow_mut().observe(state, received);
        }
        let state = self.visibility.borrow().state_at(Instant::now());
        let state_changed = self.privacy_state.replace(state) != state;
        let hidden = self.hide_when_locked.get() && state != LockState::Unlocked;
        changed | (self.hidden.replace(hidden) != hidden) | (hidden && state_changed)
    }

    pub fn set_hide_when_locked(&self, enabled: bool) {
        self.hide_when_locked.set(enabled);
        self.hidden
            .set(enabled && !self.visibility.borrow().allows_host(Instant::now()));
    }

    pub fn host_hidden(&self) -> bool {
        self.hidden.get()
    }

    pub fn privacy_message(&self) -> &'static str {
        if !self.host_hidden() {
            return "";
        }
        match self.visibility.borrow().state_at(Instant::now()) {
            LockState::Locked => "Host calendar hidden · Computer locked",
            _ => "Host calendar hidden · Waiting for unlocked computer",
        }
    }

    pub fn has_snapshot(&self) -> bool {
        self.contents.borrow().loaded
    }

    pub fn updated_at(&self) -> Option<DateTime<Utc>> {
        let contents = self.contents.borrow();
        if self.host_hidden() {
            contents.direct_updated_at
        } else {
            contents.updated_at
        }
    }

    pub fn home(&self, now: DateTime<Utc>) -> HomeAgenda {
        let contents = self.contents.borrow();
        home(
            contents.visible(self.host_hidden()),
            now,
            crate::settings::zone(),
        )
    }

    #[cfg(test)]
    pub fn events_on(&self, day: NaiveDate) -> Vec<Event> {
        let contents = self.contents.borrow();
        project(
            contents.visible(self.host_hidden()),
            day,
            crate::settings::zone(),
        )
    }

    pub fn upcoming(&self, now: DateTime<Utc>, days: u32) -> Upcoming {
        let contents = self.contents.borrow();
        upcoming(
            contents.visible(self.host_hidden()),
            now,
            crate::settings::zone(),
            days,
        )
    }
}

fn home(events: &[CalendarEvent], now: DateTime<Utc>, zone: chrono_tz::Tz) -> HomeAgenda {
    let day = now.with_timezone(&zone).date_naive();
    let mut today = project(events, day, zone);
    today.sort_by(|a, b| {
        a.is_all_day()
            .cmp(&b.is_all_day())
            .then_with(|| a.compare_start(b))
    });
    let (past, current) = today.into_iter().partition(|event| !event.pending_at(now));
    HomeAgenda {
        past,
        current,
        tomorrow: project(events, day + CalendarDuration::days(1), zone),
    }
}

fn upcoming(
    events: &[CalendarEvent],
    now: DateTime<Utc>,
    zone: chrono_tz::Tz,
    days: u32,
) -> Upcoming {
    let today = now.with_timezone(&zone).date_naive();
    let end_day = today + CalendarDuration::days(i64::from(days));
    let pending = events.iter().filter(|event| {
        pending_at(&event.time, now, zone)
            && match event.time {
                EventTime::Timed { start, .. } => start.with_timezone(&zone).date_naive() < end_day,
                EventTime::AllDay { start, .. } => start < end_day,
            }
    });
    let event_count = pending.clone().count();
    let days = (0..days)
        .filter_map(|offset| {
            let date = today + CalendarDuration::days(i64::from(offset));
            let events = project(pending.clone(), date, zone);
            (date == today || !events.is_empty()).then_some(UpcomingDay { date, events })
        })
        .collect();
    Upcoming { days, event_count }
}

struct Input {
    path: PathBuf,
    snapshot: Option<Snapshot>,
    error: Option<String>,
}

impl Input {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            snapshot: None,
            error: None,
        }
    }

    fn refresh(&mut self) -> bool {
        match Snapshot::load(&self.path) {
            Ok(snapshot) => {
                self.error = None;
                if self.snapshot.as_ref() == Some(&snapshot) {
                    return false;
                }
                self.snapshot = Some(snapshot);
                true
            }
            Err(error) => {
                let message = format!(
                    "Calendar {} unavailable. Retaining last good data. {error:#}",
                    self.path.display()
                );
                if self.error.as_ref() != Some(&message) {
                    eprintln!("{message}");
                    self.error = Some(message);
                }
                false
            }
        }
    }
}

fn project<'a>(
    events: impl IntoIterator<Item = &'a CalendarEvent>,
    day: NaiveDate,
    zone: chrono_tz::Tz,
) -> Vec<Event> {
    let midnight = day.and_hms_opt(0, 0, 0).unwrap();
    let next_day = midnight + chrono::Duration::days(1);
    let mut events: Vec<_> = events
        .into_iter()
        .filter_map(|event| {
            let event = Event {
                title: event.title.clone(),
                detail: event.detail.clone(),
                time: event.time.clone(),
                zone,
            };
            (event.local_start() < next_day && event.local_end() > midnight).then_some(event)
        })
        .collect();
    events.sort_by(Event::compare_start);
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timed(start: &str, end: &str) -> CalendarEvent {
        CalendarEvent {
            id: "id".into(),
            uid: None,
            title: "Real meeting".into(),
            detail: String::new(),
            time: EventTime::Timed {
                start: start.parse().unwrap(),
                end: end.parse().unwrap(),
            },
        }
    }

    #[test]
    fn local_days_dst_and_overnight_events() {
        let events = vec![timed("2026-11-02T07:30:00Z", "2026-11-02T09:00:00Z")];
        let zone = chrono_tz::America::Los_Angeles;
        assert_eq!(
            project(&events, "2026-11-01".parse().unwrap(), zone)[0]
                .local_start()
                .to_string(),
            "2026-11-01 23:30:00"
        );
        assert_eq!(
            project(&events, "2026-11-02".parse().unwrap(), zone).len(),
            1
        );
        assert!(project(&events, "2026-11-03".parse().unwrap(), zone).is_empty());
        let fall_back = vec![timed("2026-11-01T08:30:00Z", "2026-11-01T10:30:00Z")];
        let shown = project(&fall_back, "2026-11-01".parse().unwrap(), zone);
        assert_eq!(shown[0].local_start().to_string(), "2026-11-01 01:30:00");
        assert_eq!(shown[0].local_end().to_string(), "2026-11-01 02:30:00");
    }

    #[test]
    fn all_day_dates_are_not_shifted_and_end_is_exclusive() {
        let event = CalendarEvent {
            time: EventTime::AllDay {
                start: "2026-11-02".parse().unwrap(),
                end: "2026-11-04".parse().unwrap(),
            },
            ..timed("2026-11-02T18:00:00Z", "2026-11-02T19:00:00Z")
        };
        for zone in [chrono_tz::America::Los_Angeles, chrono_tz::Asia::Kolkata] {
            assert!(project(
                std::slice::from_ref(&event),
                "2026-11-01".parse().unwrap(),
                zone
            )
            .is_empty());
            assert!(project(
                std::slice::from_ref(&event),
                "2026-11-02".parse().unwrap(),
                zone
            )[0]
            .is_all_day());
            assert_eq!(
                project(
                    std::slice::from_ref(&event),
                    "2026-11-03".parse().unwrap(),
                    zone
                )
                .len(),
                1
            );
            assert!(project(
                std::slice::from_ref(&event),
                "2026-11-04".parse().unwrap(),
                zone
            )
            .is_empty());
        }
    }

    #[test]
    fn upcoming_groups_only_pending_events_in_the_requested_local_days() {
        let zone = chrono_tz::America::Los_Angeles;
        let now: DateTime<Utc> = "2026-11-02T18:00:00Z".parse().unwrap();
        let today: NaiveDate = "2026-11-02".parse().unwrap();
        let mut spanning = timed("2026-11-01T22:00:00Z", "2026-11-03T19:00:00Z");
        spanning.title = "Overnight event".into();
        let holiday = CalendarEvent {
            title: "Holiday".into(),
            time: EventTime::AllDay {
                start: today,
                end: today + CalendarDuration::days(3),
            },
            ..spanning.clone()
        };
        let events = vec![
            timed("2026-11-02T17:00:00Z", "2026-11-02T18:00:00Z"),
            spanning,
            holiday,
            timed("2026-11-09T08:00:00Z", "2026-11-09T09:00:00Z"),
        ];
        let shown = upcoming(&events, now, zone, 7);
        assert_eq!(shown.event_count, 2);
        assert_eq!(
            shown.days.iter().map(|day| day.date).collect::<Vec<_>>(),
            vec![
                today,
                today + CalendarDuration::days(1),
                today + CalendarDuration::days(2)
            ]
        );
        assert_eq!(shown.days[0].events.len(), 2);
        assert_eq!(shown.days[0].events[0].title, "Holiday");
        assert_eq!(shown.days[0].events[1].title, "Overnight event");
        assert!(shown.days[0].events[1].happening_at(now));
        assert_eq!(shown.days[2].events.len(), 1);
    }

    #[test]
    fn upcoming_uses_instants_during_the_repeated_dst_hour() {
        let zone = chrono_tz::America::Los_Angeles;
        let now = "2026-11-01T09:00:00Z".parse().unwrap();
        let ended = timed("2026-11-01T07:30:00Z", "2026-11-01T08:30:00Z");
        let mut active = timed("2026-11-01T08:45:00Z", "2026-11-01T09:45:00Z");
        active.title = "Active across clock change".into();
        let mut future = timed("2026-11-01T09:15:00Z", "2026-11-01T10:15:00Z");
        future.title = "Second occurrence of one o'clock".into();
        let shown = upcoming(&[ended, future, active], now, zone, 7);
        assert_eq!(shown.event_count, 2);
        assert_eq!(shown.days[0].events[0].title, "Active across clock change");
        assert_eq!(
            shown.days[0].events[0].local_start().time().to_string(),
            "01:45:00"
        );
        assert!(shown.days[0].events[0].happening_at(now));
        assert_eq!(
            shown.days[0].events[1].local_start().time().to_string(),
            "01:15:00"
        );
        assert!(!shown.days[0].events[1].happening_at(now));
    }

    #[test]
    fn home_and_calendar_agree_during_the_repeated_dst_hour() {
        let zone = chrono_tz::America::Los_Angeles;
        let now = "2026-11-01T09:00:00Z".parse().unwrap();
        let ended = timed("2026-11-01T07:30:00Z", "2026-11-01T08:30:00Z");
        let mut active = timed("2026-11-01T08:45:00Z", "2026-11-01T09:45:00Z");
        active.title = "Active".into();
        let mut future = timed("2026-11-01T09:15:00Z", "2026-11-01T10:15:00Z");
        future.title = "Future".into();
        let records = [ended, future, active];
        let home = home(&records, now, zone);
        let calendar = upcoming(&records, now, zone, 7);
        assert_eq!(home.past.len(), 1);
        assert_eq!(home.current.len(), calendar.event_count);
        assert_eq!(home.current[0].title, "Active");
        assert_eq!(home.current[1].title, "Future");
    }

    #[test]
    fn countdowns_measure_elapsed_minutes_across_both_dst_transitions() {
        let zone = chrono_tz::America::Los_Angeles;
        for (now, start, end, expected) in [
            (
                "2026-11-01T08:45:00Z",
                "2026-11-01T09:15:00Z",
                "2026-11-01T10:15:00Z",
                31,
            ),
            (
                "2027-03-14T09:50:00Z",
                "2027-03-14T10:10:00Z",
                "2027-03-14T11:10:00Z",
                21,
            ),
        ] {
            let now = now.parse().unwrap();
            let records = [timed(start, end)];
            let agenda = home(&records, now, zone);
            assert_eq!(agenda.current.len(), 1);
            assert_eq!(agenda.current[0].minutes_until(now), Some(expected));
            assert!(!agenda.current[0].happening_at(now));
            let end = end.parse().unwrap();
            assert!(home(&records, end, zone).current.is_empty());
            assert_eq!(upcoming(&records, end, zone, 7).event_count, 0);
        }
    }

    #[test]
    fn upcoming_keeps_today_when_the_next_event_is_later() {
        let now = "2026-11-02T18:00:00Z".parse().unwrap();
        let shown = upcoming(
            &[timed("2026-11-04T18:00:00Z", "2026-11-04T19:00:00Z")],
            now,
            chrono_tz::UTC,
            7,
        );
        assert_eq!(shown.event_count, 1);
        assert_eq!(shown.days.len(), 2);
        assert_eq!(shown.days[0].date.to_string(), "2026-11-02");
        assert!(shown.days[0].events.is_empty());
        assert_eq!(shown.days[1].date.to_string(), "2026-11-04");
    }

    #[test]
    fn privacy_transitions_preserve_cached_source_projections() {
        use LockState::{Locked, Unknown, Unlocked};

        let fresh = Duration::ZERO;
        let now: DateTime<Utc> = "2026-11-02T18:30:00Z".parse().unwrap();
        let direct_updated_at = now - CalendarDuration::hours(2);
        let host_updated_at = now - CalendarDuration::hours(1);
        let (_tx, updates) = mpsc::sync_channel(1);
        let direct = timed("2026-11-02T18:00:00Z", "2026-11-02T19:00:00Z");
        let mut secret = direct.clone();
        secret.title = "Private work calendar".into();
        let calendar = Calendar {
            contents: RefCell::new(Contents {
                loaded: true,
                events: vec![direct.clone(), secret],
                direct_events: vec![direct],
                updated_at: Some(host_updated_at),
                direct_updated_at: Some(direct_updated_at),
            }),
            visibility: RefCell::new(Visibility::default()),
            hide_when_locked: Cell::new(false),
            hidden: Cell::new(false),
            privacy_state: Cell::new(LockState::Unknown),
            updates,
            host_updates: Arc::new(Mutex::new(None)),
        };
        let day = now.with_timezone(&crate::settings::zone()).date_naive();
        calendar.set_hide_when_locked(true);
        for (case, state, age, hidden, changed) in [
            ("startup", None, fresh, true, false),
            ("fresh unlock", Some(Unlocked), fresh, false, true),
            (
                "heartbeat needs no rebuild",
                Some(Unlocked),
                fresh,
                false,
                false,
            ),
            (
                "queued expired unlock hides cached host",
                Some(Unlocked),
                host_state::LEASE,
                true,
                true,
            ),
            ("unlock restores cache", Some(Unlocked), fresh, false, true),
            ("restart", None, fresh, true, true),
            ("unlock after restart", Some(Unlocked), fresh, false, true),
            ("locked", Some(Locked), fresh, true, true),
            (
                "unknown changes hidden reason",
                Some(Unknown),
                fresh,
                true,
                true,
            ),
        ] {
            if let Some(state) = state {
                *calendar.host_updates.lock().unwrap() = Some((state, Instant::now() - age));
            } else {
                *calendar.visibility.borrow_mut() = Visibility::default();
            }
            assert_eq!(calendar.poll(), changed, "{case}");
            assert_eq!(calendar.host_hidden(), hidden, "{case}");
            assert!(calendar.has_snapshot(), "{case}");
            assert_eq!(
                calendar.updated_at(),
                Some(if hidden {
                    direct_updated_at
                } else {
                    host_updated_at
                }),
                "{case}"
            );
            let expected = if hidden {
                vec!["Real meeting"]
            } else {
                vec!["Real meeting", "Private work calendar"]
            };
            let agenda = calendar.home(now);
            let upcoming = calendar.upcoming(now, 7);
            assert_eq!(upcoming.event_count, expected.len(), "{case}");
            for events in [
                calendar.events_on(day),
                agenda.current,
                upcoming
                    .days
                    .into_iter()
                    .flat_map(|day| day.events)
                    .collect(),
            ] {
                assert_eq!(
                    events
                        .iter()
                        .map(|event| event.title.as_str())
                        .collect::<Vec<_>>(),
                    expected,
                    "{case}"
                );
            }
            assert_eq!(
                calendar.privacy_message(),
                if !hidden {
                    ""
                } else if state == Some(LockState::Locked) {
                    "Host calendar hidden · Computer locked"
                } else {
                    "Host calendar hidden · Waiting for unlocked computer"
                },
                "{case}"
            );
        }
        calendar.set_hide_when_locked(false);
        assert_eq!(calendar.events_on(day).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn privacy_observation_continues_while_calendar_read_is_blocked() {
        let directory = tempfile::tempdir().unwrap();
        let host = directory.path().join("blocked.json");
        assert!(std::process::Command::new("mkfifo")
            .arg(&host)
            .status()
            .unwrap()
            .success());
        let status_path = host_state::path_for_calendar(&host);
        let calendar = Calendar::from_paths(host.clone(), None).unwrap();
        calendar.set_hide_when_locked(true);
        let (opened, ready) = mpsc::channel();
        std::thread::spawn(move || {
            let writer = std::fs::OpenOptions::new().write(true).open(host).unwrap();
            let _ = opened.send(writer);
        });
        let writer = ready.recv_timeout(Duration::from_secs(5)).unwrap();
        // Keeping the FIFO writer open without bytes holds the real snapshot loader.
        for state in [LockState::Unlocked, LockState::Locked] {
            let deadline = Instant::now() + Duration::from_secs(6);
            loop {
                let mut packet = Status::new(state).encode().unwrap();
                packet.push(b'\n');
                host_state::receive(&packet[..], Vec::new(), &status_path).unwrap();
                calendar.poll();
                if calendar.visibility.borrow().state_at(Instant::now()) == state {
                    break;
                }
                assert!(Instant::now() < deadline, "privacy waited for calendar I/O");
                std::thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(calendar.host_hidden(), state != LockState::Unlocked);
            assert!(!calendar.has_snapshot());
        }
        drop(writer);
    }

    #[test]
    fn invalid_updates_retain_loaded_data_and_empty_snapshot_clears_it() {
        let path = std::env::temp_dir().join(format!("frame-calendar-{}.json", std::process::id()));
        let snapshot = Snapshot::new(vec![calendar_sync::Source {
            id: "host".into(),
            events: vec![timed("2026-11-02T18:00:00Z", "2026-11-02T19:00:00Z")],
        }]);
        snapshot.save(&path).unwrap();
        let mut input = Input::new(path.clone());
        assert!(input.refresh());
        assert!(!input.refresh());
        std::fs::write(&path, "broken").unwrap();
        assert!(!input.refresh());
        assert_eq!(input.snapshot, Some(snapshot));
        Snapshot::new(vec![]).save(&path).unwrap();
        assert!(input.refresh());
        assert!(input.snapshot.unwrap().sources.is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
