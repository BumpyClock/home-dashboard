use chrono::{Duration, Local, NaiveDate, NaiveDateTime, NaiveTime};
use crate::household::PersonIdx;

#[derive(Clone, Debug)]
pub struct Event {
    pub start: NaiveDateTime,
    pub end: Option<NaiveDateTime>,
    pub title: String,
    pub detail: String,
    pub people: Vec<PersonIdx>,
    pub heads_up: Option<String>,
}

impl Event {
    pub fn ends(&self) -> NaiveDateTime {
        self.end.unwrap_or(self.start + Duration::minutes(30))
    }
}

pub trait CalendarSource {
    fn events_on(&self, day: NaiveDate) -> Vec<Event>;
}

pub struct DemoCalendar;

impl CalendarSource for DemoCalendar {
    fn events_on(&self, day: NaiveDate) -> Vec<Event> {
        const ADITYA: PersonIdx = PersonIdx::new(0);
        const NEHA: PersonIdx = PersonIdx::new(1);
        const KABIR: PersonIdx = PersonIdx::new(2);
        const MIRA: PersonIdx = PersonIdx::new(3);
        let at = |h: u32, m: u32| day.and_time(NaiveTime::from_hms_opt(h, m, 0).unwrap());
        let ev = |start: NaiveDateTime, end: Option<NaiveDateTime>, title: &str, detail: &str, people: &[PersonIdx]| Event {
            start,
            end,
            title: title.into(),
            detail: detail.into(),
            people: people.to_vec(),
            heads_up: None,
        };
        let today = Local::now().date_naive();
        let mut events = if day == today + Duration::days(1) {
            vec![
                ev(at(8, 0), Some(at(8, 30)), "School drop-off", "Aditya driving", &[ADITYA, KABIR, MIRA]),
                ev(at(17, 30), None, "Kabir's PTM", "Room 4B", &[KABIR, NEHA]),
                ev(at(20, 0), None, "Book club", "At Priya's", &[NEHA]),
            ]
        } else {
            vec![
                Event {
                    heads_up: Some("School run leaves".into()),
                    ..ev(at(8, 0), Some(at(8, 30)), "School drop-off", "Neha driving", &[NEHA, KABIR, MIRA])
                },
                ev(at(10, 30), None, "Plumber visit", "Kitchen sink", &[NEHA]),
                ev(at(13, 0), None, "Video call with Dadi", "WhatsApp", &[ADITYA, NEHA]),
                ev(at(16, 0), Some(at(17, 0)), "Kabir's swim class", "HSR pool · Aditya picks up", &[ADITYA, KABIR]),
                ev(at(18, 0), Some(at(18, 45)), "Mira's piano lesson", "Mrs. Iyer's", &[MIRA]),
                ev(at(19, 30), Some(at(21, 0)), "Family dinner", "Rajma night", &[ADITYA, NEHA, KABIR, MIRA]),
            ]
        };
        events.sort_by_key(|e| e.start);
        events
    }
}
