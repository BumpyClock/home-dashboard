//! Demo household: people, chores, shared list. Replace with real
//! sources (Home Assistant, a lists API) later.

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

pub struct Chore {
    pub name: &'static str,
    pub person: usize,
    /// Mon..Sun this week.
    pub done: [bool; 7],
}

pub fn demo_chores(today: usize) -> Vec<Chore> {
    // Earlier days mostly done; today open so there's something to tap.
    let week = |pattern: [bool; 7]| {
        let mut d = pattern;
        for (i, v) in d.iter_mut().enumerate() {
            if i >= today { *v = false; }
        }
        d
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
