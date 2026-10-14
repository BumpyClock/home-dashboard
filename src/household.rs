use chrono::Weekday;

#[derive(Clone, Copy)]
pub struct Person {
    pub name: &'static str,
    pub initial: &'static str,
    pub color: u32,
}

pub const HOUSEHOLD: &str = "Sharmas";

pub const PEOPLE: [Person; 4] = [
    Person { name: "Aditya", initial: "A", color: 0x4B7AA3 },
    Person { name: "Neha", initial: "N", color: 0xC9532D },
    Person { name: "Kabir", initial: "K", color: 0x5F8460 },
    Person { name: "Mira", initial: "M", color: 0x8A63A8 },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PersonIdx(usize);

impl PersonIdx {
    pub const fn new(index: usize) -> Self { Self(index) }

    pub fn person(self) -> Person { PEOPLE[self.0] }
}

pub const MONDAY_FIRST: [Weekday; 7] = [
    Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri, Weekday::Sat, Weekday::Sun,
];

pub struct WeekDays([bool; 7]);

impl WeekDays {
    pub fn from_monday_first(done: [bool; 7]) -> Self { Self(done) }

    pub fn is_done(&self, day: Weekday) -> bool { self.0[day.num_days_from_monday() as usize] }

    pub fn toggle(&mut self, day: Weekday) {
        let day = day.num_days_from_monday() as usize;
        self.0[day] = !self.0[day];
    }

    pub fn clear_rest_of_week(&mut self, from: Weekday) {
        let from = from.num_days_from_monday() as usize;
        for (day, done) in self.0.iter_mut().enumerate() {
            if day >= from { *done = false; }
        }
    }
}

pub struct Chore {
    pub name: &'static str,
    pub person: usize,
    pub done: WeekDays,
}

pub fn demo_chores(today: Weekday) -> Vec<Chore> {
    let week = |pattern: [bool; 7]| {
        let mut done = WeekDays::from_monday_first(pattern);
        done.clear_rest_of_week(today);
        done
    };
    vec![
        Chore { name: "Walk Bruno", person: 2, done: week([true, true, true, false, true, true, true]) },
        Chore { name: "Water plants", person: 3, done: week([true, false, true, true, true, true, true]) },
        Chore { name: "Dishes", person: 0, done: week([true, true, false, true, true, true, true]) },
        Chore { name: "Take out trash", person: 2, done: week([true, true, true, true, false, true, true]) },
    ]
}

pub struct ListEntry {
    pub title: &'static str,
    pub person: usize,
    pub done: bool,
}

pub fn demo_list() -> Vec<ListEntry> {
    vec![
        ListEntry { title: "Milk, eggs, bread", person: 1, done: false },
        ListEntry { title: "Gift for Dadi's birthday", person: 3, done: false },
        ListEntry { title: "Swim goggles for Kabir", person: 2, done: false },
        ListEntry { title: "Dish soap", person: 0, done: false },
        ListEntry { title: "Birthday candles", person: 1, done: false },
    ]
}
