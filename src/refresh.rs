use chrono::{NaiveDateTime, Timelike};
use slint::{Model, ModelRc, VecModel};
use std::time::Duration;

use crate::{AgendaRow, ChoreRow};

pub fn until_next_minute(now: NaiveDateTime) -> Duration {
    let elapsed_ms = u64::from(now.second()) * 1000 + u64::from(now.nanosecond()) / 1_000_000;
    Duration::from_millis(60_000_u64.saturating_sub(elapsed_ms).max(1))
}

pub fn sync<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    sync_by(model, rows, PartialEq::eq);
}

pub fn sync_by<T: Clone + 'static>(model: &VecModel<T>, rows: Vec<T>, equal: impl Fn(&T, &T) -> bool) {
    let count = rows.len();
    for (index, row) in rows.into_iter().enumerate() {
        match model.row_data(index) {
            Some(old) if equal(&old, &row) => {}
            Some(_) => model.set_row_data(index, row),
            None => model.push(row),
        }
    }
    while model.row_count() > count { model.remove(model.row_count() - 1); }
}

fn same_rows<T: Clone + PartialEq + 'static>(left: &ModelRc<T>, right: &ModelRc<T>) -> bool {
    left.row_count() == right.row_count() && left.iter().eq(right.iter())
}

pub fn same_agenda(left: &AgendaRow, right: &AgendaRow) -> bool {
    left.time == right.time && left.title == right.title && left.meta == right.meta
        && left.next == right.next && left.past == right.past
        && same_rows(&left.people, &right.people)
}

pub fn same_chore(left: &ChoreRow, right: &ChoreRow) -> bool {
    left.name == right.name && left.who == right.who && left.tint == right.tint
        && same_rows(&left.days, &right.days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Avatar, DayDot, ListItem};
    use slint::Color;

    #[test]
    fn scheduling_reaches_next_boundary_without_early_wakeup() {
        for (time, milliseconds) in [
            ("2026-10-13 12:34:00", 60_000),
            ("2026-10-13 12:34:12.3456", 47_655),
            ("2026-10-13 23:59:59.9995", 1),
            ("2026-10-14 00:00:00.001", 59_999),
        ] {
            let now = NaiveDateTime::parse_from_str(time, "%Y-%m-%d %H:%M:%S%.f").unwrap();
            let delay = until_next_minute(now);
            assert_eq!(delay.as_millis(), milliseconds);
            let next = now + chrono::Duration::from_std(delay).unwrap();
            assert_eq!(next.second(), 0);
            assert!(next.nanosecond() < 1_000_000);
        }
    }

    fn agenda(initials: &[&str]) -> AgendaRow {
        AgendaRow {
            title: "School".into(),
            people: ModelRc::new(VecModel::from(initials.iter().map(|initial| Avatar {
                initial: (*initial).into(), tint: Color::from_rgb_u8(1, 2, 3),
            }).collect::<Vec<_>>())),
            ..Default::default()
        }
    }

    #[test]
    fn unchanged_agenda_keeps_nested_model_but_people_changes_publish() {
        let original = agenda(&["A", "N"]);
        let model = VecModel::from(vec![original.clone()]);
        sync_by(&model, vec![agenda(&["A", "N"])], same_agenda);
        assert_eq!(model.row_data(0).unwrap().people, original.people);
        sync_by(&model, vec![agenda(&["N", "A"])], same_agenda);
        assert_eq!(model.row_data(0).unwrap().people.row_data(0).unwrap().initial, "N");
        let mut recolored = model.row_data(0).unwrap();
        recolored.people = ModelRc::new(VecModel::from(vec![Avatar {
            initial: "N".into(), tint: Color::from_rgb_u8(9, 8, 7),
        }, Avatar { initial: "A".into(), tint: Color::from_rgb_u8(1, 2, 3) }]));
        sync_by(&model, vec![recolored.clone()], same_agenda);
        assert_eq!(model.row_data(0).unwrap().people, recolored.people);
    }

    #[test]
    fn chore_refresh_keeps_unchanged_days_and_publishes_completion() {
        let make = |state| ChoreRow {
            name: "Plants".into(),
            days: ModelRc::new(VecModel::from(vec![DayDot { state, ..Default::default() }])),
            ..Default::default()
        };
        let original = make(2);
        let model = VecModel::from(vec![original.clone()]);
        sync_by(&model, vec![make(2)], same_chore);
        assert_eq!(model.row_data(0).unwrap().days, original.days);
        sync_by(&model, vec![make(1)], same_chore);
        assert_eq!(model.row_data(0).unwrap().days.row_data(0).unwrap().state, 1);
    }

    #[test]
    fn list_refresh_updates_values_and_handles_growth_and_removal() {
        let item = |title: &str, done| ListItem { title: title.into(), done, ..Default::default() };
        let model = VecModel::from(vec![item("Milk", false)]);
        sync(&model, vec![item("Milk", true), item("Bread", false)]);
        assert!(model.row_data(0).unwrap().done);
        assert_eq!(model.row_count(), 2);
        sync(&model, vec![item("Bread", false)]);
        assert_eq!(model.row_count(), 1);
        assert_eq!(model.row_data(0).unwrap().title, "Bread");
        sync(&model, vec![]);
        assert_eq!(model.row_count(), 0);
    }
}
