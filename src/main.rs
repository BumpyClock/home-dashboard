// Family Frame — Slint dashboard prototype matching the pen.dev
// "Family Dashboard" design.
//
// Data seams (swap later for real feeds):
//   calendar.rs  → `CalendarSource` (DemoCalendar today)
//   household.rs → people, chores, shared list (demo)
//   photos.rs    → real photos, decoded lazily around the one shown
//   weather.rs   → live Open-Meteo
//   settings.rs  → theme / fonts / text size / clock / motion, persisted
mod calendar;
mod frame_metrics;
mod household;
mod photos;
mod photo_load_state;
#[cfg(all(test, feature = "desktop"))]
mod photo_stage_tests;
mod refresh;
mod screensaver;
mod settings;
mod weather;
#[cfg(any(test, all(feature = "frame", target_os = "linux")))]
mod display_power;
#[cfg(any(test, all(feature = "frame", target_os = "linux")))]
mod orientation;
#[cfg(all(feature = "frame", target_os = "linux"))]
mod shutdown;

use calendar::{CalendarSource, DemoCalendar, Event};
use chrono::{Datelike, Duration as CDur, Local, NaiveDateTime, Timelike};

use household::PEOPLE;
use settings::Settings;
use refresh::{same_agenda, same_chore, sync, sync_by, until_next_minute};
use slint::{Color, ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

slint::include_modules!();

/// Local time, optionally shifted by FRAME_NOW=HH:MM (preview other times of day).
fn now() -> NaiveDateTime {
    use std::sync::OnceLock;
    static OFFSET: OnceLock<CDur> = OnceLock::new();
    let real = Local::now().naive_local();
    real + *OFFSET.get_or_init(|| {
        std::env::var("FRAME_NOW").ok()
            .and_then(|v| chrono::NaiveTime::parse_from_str(&v, "%H:%M").ok())
            .map_or(CDur::zero(), |t| real.date().and_time(t) - real)
    })
}

fn c(hex: u32) -> Color { Color::from_rgb_u8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8) }
fn s(v: impl AsRef<str>) -> SharedString { SharedString::from(v.as_ref()) }

macro_rules! set_changed {
    ($object:ident, $get:ident, $set:ident, $value:expr) => {{
        let value = $value;
        if $object.$get() != value { $object.$set(value); }
    }};
}

fn fmt_time(t: NaiveDateTime, clock24: bool) -> String {
    if clock24 { return t.format("%H:%M").to_string(); }
    let h = t.hour() % 12;
    format!("{}:{:02} {}", if h == 0 { 12 } else { h }, t.minute(), if t.hour() < 12 { "am" } else { "pm" })
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

struct Models {
    agenda_past: Rc<VecModel<AgendaRow>>,
    agenda_next: Rc<VecModel<AgendaRow>>,
    tomorrow: Rc<VecModel<MiniEvent>>,
    list: Rc<VecModel<ListItem>>,
    chores: Rc<VecModel<ChoreRow>>,
}

struct State {
    settings: Cell<Settings>,
    chores: RefCell<Vec<household::Chore>>,
    list: RefCell<Vec<household::ListEntry>>,
    calendar: Box<dyn CalendarSource>,
    models: Models,
}

fn refresh_clock(app: &AppWindow, st: &State) {
    let now = now();
    let f = app.global::<Frame>();
    let clock24 = st.settings.get().clock24;
    if clock24 {
        set_changed!(f, get_clock, set_clock, s(now.format("%H:%M").to_string()));
    } else {
        let h = now.hour() % 12;
        set_changed!(f, get_clock, set_clock, s(format!("{}:{:02}", if h == 0 { 12 } else { h }, now.minute())));
    }
    set_changed!(f, get_ampm, set_ampm, s(if now.hour() < 12 { "am" } else { "pm" }));
    set_changed!(f, get_now_short, set_now_short, s(fmt_time(now, clock24)));
    set_changed!(f, get_dateline, set_dateline, s(now.format("%A · %-d %B").to_string().to_uppercase()));
}

fn agenda_row(e: &Event, clock24: bool, next: bool, past: bool) -> AgendaRow {
    let range = match e.end {
        Some(end) => format!("{} – {}", fmt_time(e.start, clock24), fmt_time(end, clock24)),
        None => fmt_time(e.start, clock24),
    };
    let people: Vec<Avatar> = e.people.iter()
        .map(|&p| Avatar { initial: s(PEOPLE[p].initial), tint: c(PEOPLE[p].color) })
        .collect();
    AgendaRow {
        time: s(fmt_time(e.start, clock24)),
        title: s(&e.title),
        meta: s(if e.detail.is_empty() { range } else { format!("{} · {range}", e.detail) }),
        people: ModelRc::new(VecModel::from(people)),
        next,
        past,
    }
}

fn refresh_calendar(app: &AppWindow, st: &State) {
    let f = app.global::<Frame>();
    let now = now();
    let clock24 = st.settings.get().clock24;
    let today = now.date();
    let events = st.calendar.events_on(today);
    let tomorrow = st.calendar.events_on(today + CDur::days(1));

    let greet = match now.hour() { 0..=11 => "Good morning", 12..=16 => "Good afternoon", _ => "Good evening" };
    set_changed!(f, get_greeting, set_greeting, s(format!("{greet}, {}.", household::HOUSEHOLD)));

    // first event not yet over = highlighted; the now line splits ended from the rest
    let next_i = events.iter().position(|e| e.ends() > now);
    let split = next_i.unwrap_or(events.len());
    let row = |(i, e): (usize, &Event)| agenda_row(e, clock24, Some(i) == next_i, e.ends() <= now);
    sync_by(&st.models.agenda_past, events.iter().enumerate().take(split).map(row).collect(), same_agenda);
    sync_by(&st.models.agenda_next, events.iter().enumerate().skip(split).map(row).collect(), same_agenda);
    set_changed!(f, get_agenda_done, set_agenda_done, s(if events.is_empty() { "Nothing on the calendar today." } else { "That's everything for today." }));
    set_changed!(f, get_agenda_done_sub, set_agenda_done_sub, s(match tomorrow.first() {
        Some(e) => format!("Tomorrow starts with {} at {}.", e.title, fmt_time(e.start, clock24)),
        None => "Tomorrow is clear too.".into(),
    }));

    let mut who: Vec<usize> = events.iter().flat_map(|e| e.people.clone()).collect();
    who.sort();
    who.dedup();
    set_changed!(f, get_agenda_summary, set_agenda_summary, s(format!("{} · {}", plural(events.len(), "event"), plural(who.len(), "person").replace("persons", "people"))));

    // header "next up" line
    let (text, urgent) = match next_i.map(|i| &events[i]) {
        Some(e) if e.start <= now => (format!("{} — happening now", e.title), false),
        Some(e) => {
            let mins = (e.start - now).num_minutes().max(0) + 1;
            let label = e.heads_up.clone().unwrap_or_else(|| e.title.clone());
            if mins < 60 { (format!("{label} in {mins} min"), mins <= 15) }
            else { (format!("{label} at {}", fmt_time(e.start, clock24)), false) }
        }
        None => match tomorrow.first() {
            Some(e) => (format!("Tomorrow: {} at {}", e.title, fmt_time(e.start, clock24)), false),
            None => ("Nothing else planned today".into(), false),
        },
    };
    set_changed!(f, get_next_up, set_next_up, s(text));
    set_changed!(f, get_urgent, set_urgent, urgent || std::env::var("FRAME_FORCE_URGENT").is_ok());

    let tday = today + CDur::days(1);
    set_changed!(f, get_tomorrow_label, set_tomorrow_label, s(format!("TOMORROW · {}", tday.format("%a").to_string().to_uppercase())));
    set_changed!(f, get_tomorrow_count, set_tomorrow_count, s(plural(tomorrow.len(), "event")));
    let minis: Vec<MiniEvent> = tomorrow.iter().take(3).map(|e| MiniEvent {
        time: s(fmt_time(e.start, clock24)),
        title: s(&e.title),
        tint: c(e.people.first().map_or(0x6F6A61, |&p| PEOPLE[p].color)),
    }).collect();
    sync(&st.models.tomorrow, minis);
}

fn refresh_lists(app: &AppWindow, st: &State) {
    let f = app.global::<Frame>();
    let list = st.list.borrow();
    let open = list.iter().filter(|e| !e.done).count();
    set_changed!(f, get_list_label, set_list_label, s(format!("SHARED LIST · {open} OPEN")));
    // the card shows three rows; the header counts everything open
    sync(&st.models.list, list.iter().take(3).map(|e| ListItem {
        title: s(e.title), tint: c(PEOPLE[e.person].color), done: e.done,
    }).collect());

    let today = now().weekday().num_days_from_monday() as usize;
    set_changed!(f, get_today_index, set_today_index, today as i32);
    sync_by(&st.models.chores, st.chores.borrow().iter().map(|ch| {
        let tint = c(PEOPLE[ch.person].color);
        let days: Vec<DayDot> = (0..7).map(|d| DayDot {
            state: if ch.done[d] { 1 } else if d == today { 2 } else { 0 },
            tint,
        }).collect();
        ChoreRow { name: s(ch.name), who: s(PEOPLE[ch.person].name), tint, days: ModelRc::new(VecModel::from(days)) }
    }).collect(), same_chore);
}

fn apply_settings(app: &AppWindow, st: &State, animate_theme: bool) {
    let set = st.settings.get();
    let theme = app.global::<Theme>();
    let f = app.global::<Frame>();
    let body = settings::BODY_FONTS[set.body_font].1;
    let display = match settings::DISPLAY_FONTS[set.display_font].1 { "" => body, d => d };
    theme.set_font_body(s(body));
    theme.set_font_display(s(display));
    theme.set_scale(settings::SCALES[set.scale]);
    let forced_off = std::env::var("FRAME_REDUCED_MOTION").is_ok();
    theme.set_motion(set.motion < 2 && !forced_off);
    theme.set_ambient(set.motion == 0 && !forced_off);
    f.set_motion_index(set.motion as i32);
    f.set_theme_index(set.theme as i32);
    f.set_display_font_index(set.display_font as i32);
    f.set_body_font_index(set.body_font as i32);
    f.set_scale_index(set.scale as i32);
    f.set_clock24(set.clock24);

    let palette = settings::themes()[set.theme].1.clone();
    if animate_theme && theme.get_motion() && theme.get_c() != palette {
        // dip to the old page color, swap underneath, fade back
        f.set_dip(true);
        let weak = app.as_weak();
        Timer::single_shot(Duration::from_millis(210), move || {
            if let Some(app) = weak.upgrade() {
                app.global::<Theme>().set_c(palette);
                app.global::<Frame>().set_dip(false);
            }
        });
    } else {
        theme.set_c(palette);
    }
}

fn schedule_clock(weak: slint::Weak<AppWindow>, st: Rc<State>) {
    Timer::single_shot(until_next_minute(now()), move || {
        let Some(app) = weak.upgrade() else { return };
        refresh_clock(&app, &st);
        refresh_calendar(&app, &st);
        refresh_lists(&app, &st);
        schedule_clock(weak, st);
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(all(feature = "frame", target_os = "linux"))]
    let termination_signals = shutdown::block_termination()?;
    let app = AppWindow::new()?;
    let today = now().weekday().num_days_from_monday() as usize;
    let models = Models {
        agenda_past: Rc::new(VecModel::default()),
        agenda_next: Rc::new(VecModel::default()),
        tomorrow: Rc::new(VecModel::default()),
        list: Rc::new(VecModel::default()),
        chores: Rc::new(VecModel::default()),
    };
    {
        let f = app.global::<Frame>();
        f.set_photo_transition_slide(std::env::var("FRAME_PHOTO_TRANSITION").map_or(true, |v| v == "slide"));
        f.set_photo_scrim_bake(std::env::var("FRAME_PHOTO_SCRIM").map_or(cfg!(feature = "frame"), |v| v == "baked"));
        f.set_cache_dashboard(std::env::var("FRAME_CACHE_DASHBOARD").map_or(true, |v| v != "0"));
        f.set_cache_main_content(std::env::var("FRAME_CACHE_MAIN_CONTENT").map_or(cfg!(feature = "frame"), |v| v != "0"));
        f.set_benchmark_mode(std::env::var("FRAME_BENCHMARK").is_ok_and(|v| v == "1"));
        f.set_agenda_past(models.agenda_past.clone().into());
        f.set_agenda_next(models.agenda_next.clone().into());
        f.set_tomorrow(models.tomorrow.clone().into());
        f.set_list(models.list.clone().into());
        f.set_chores(models.chores.clone().into());
        f.set_weather_line(s(format!("{} · H 27° L 19°", weather::city())));
        f.set_themes(ModelRc::new(VecModel::from(settings::themes().into_iter().map(|(name, p)| ThemeSwatch {
            name: s(name), bg: p.bg, surface: p.surface, ink: p.ink, muted: p.muted, accent: p.accent, rail: p.rail,
        }).collect::<Vec<_>>())));
        let fonts = |list: &[(&str, &str)]| ModelRc::new(VecModel::from(list.iter()
            .map(|(l, fam)| FontChoice { label: s(l), family: s(fam) }).collect::<Vec<_>>()));
        f.set_display_fonts(fonts(&settings::DISPLAY_FONTS));
        f.set_body_fonts(fonts(&settings::BODY_FONTS));
        photos::init(&app);
        if let Some(tab) = std::env::var("FRAME_TAB").ok().and_then(|t| t.parse::<i32>().ok()) {
            f.set_active_tab(tab.clamp(0, 5));
        }
    }

    let st = Rc::new(State {
        settings: Cell::new(Settings::load()),
        chores: RefCell::new(household::demo_chores(today)),
        list: RefCell::new(household::demo_list()),
        calendar: Box::new(DemoCalendar),
        models,
    });

    // FRAME_SIZE=800x1280 → open at that size (e.g. preview portrait)
    if let Some((w, h)) = std::env::var("FRAME_SIZE").ok()
        .and_then(|v| v.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))) {
        app.window().set_size(slint::LogicalSize::new(w, h));
    }
    apply_settings(&app, &st, false);
    // photos: re-scale to the card's pixel size, debounced across resizes
    {
        let pending = Rc::new(Timer::default());
        let weak = app.as_weak();
        app.global::<Frame>().on_photo_size_changed(move |w, h, scrim_h| {
            if w < 1.0 || h < 1.0 { return; }
            let weak = weak.clone();
            pending.start(TimerMode::SingleShot, Duration::from_millis(250), move || {
                let Some(app) = weak.upgrade() else { return };
                let sf = app.window().scale_factor();
                let size = ((w * sf).ceil() as u32, (h * sf).ceil() as u32, (scrim_h * sf).ceil() as u32);
                photos::load_scaled(app.as_weak(), size, app.global::<Frame>().get_photo_scrim_bake());
            });
        });
        // Initial layout can settle before the callback is installed.
        let weak = app.as_weak();
        Timer::single_shot(Duration::ZERO, move || {
            let Some(app) = weak.upgrade() else { return };
            let frame = app.global::<Frame>();
            let (w, h) = (frame.get_photo_card_width(), frame.get_photo_card_height());
            if w > 0.0 && h > 0.0 {
                frame.invoke_photo_size_changed(w, h, frame.get_photo_scrim_height());
            }
        });
    }
    refresh_clock(&app, &st);
    refresh_calendar(&app, &st);
    refresh_lists(&app, &st);

    // Recompute after each tick so a delayed callback cannot accumulate drift.
    schedule_clock(app.as_weak(), st.clone());

    let f = app.global::<Frame>();
    macro_rules! on {
        ($setter:ident, |$app:ident, $st:ident, $arg:ident : $t:ty| $body:block) => {{
            let (weak, st) = (app.as_weak(), st.clone());
            f.$setter(move |$arg: $t| {
                let Some($app) = weak.upgrade() else { return };
                let $st = &st;
                $body
            });
        }};
    }
    macro_rules! setting {
        ($setter:ident, |$set:ident, $arg:ident : $t:ty| $body:block, $refresh:expr) => {
            on!($setter, |app, st, $arg: $t| {
                let mut $set = st.settings.get();
                $body
                st.settings.set($set);
                $set.save();
                apply_settings(&app, st, true);
                if $refresh { refresh_clock(&app, st); refresh_calendar(&app, st); }
            })
        };
    }
    on!(on_toggle_chore, |app, st, i: i32| {
        if let Some(ch) = st.chores.borrow_mut().get_mut(i as usize) {
            let d = now().weekday().num_days_from_monday() as usize;
            ch.done[d] = !ch.done[d];
        }
        refresh_lists(&app, st);
    });
    on!(on_toggle_item, |app, st, i: i32| {
        if let Some(e) = st.list.borrow_mut().get_mut(i as usize) { e.done = !e.done; }
        refresh_lists(&app, st);
    });
    f.on_add_item(|| eprintln!("[lists] Add tapped — item entry not built yet"));
    setting!(on_pick_theme, |set, i: i32| { set.theme = i as usize; }, false);
    setting!(on_pick_display_font, |set, i: i32| { set.display_font = i as usize; }, false);
    setting!(on_pick_body_font, |set, i: i32| { set.body_font = i as usize; }, false);
    setting!(on_pick_scale, |set, i: i32| { set.scale = i as usize; }, false);
    setting!(on_pick_clock24, |set, v: bool| { set.clock24 = v; }, true);
    setting!(on_pick_motion, |set, i: i32| { set.motion = i as usize; }, false);

    weather::spawn(app.as_weak());
    let _frame_metrics = frame_metrics::install(&app);
    let _screensaver = screensaver::install(&app);
    #[cfg(all(feature = "frame", target_os = "linux"))]
    let _display_power = {
        let guard: Option<Rc<display_power::Guard>> = display_power::start(&app).map(Rc::new);
        if let Some(guard) = &guard {
            let saver = _screensaver.clone();
            guard.on_activity(move |dismiss| saver.activity(dismiss));
            _screensaver.observe();
        }
        if let Some(guard) = &guard {
            let weak = app.as_weak();
            guard.on_night_changed(move |active| {
                if let Some(app) = weak.upgrade() { app.global::<Frame>().set_night_active(active); }
            });
        }
        guard
    };
    #[cfg(all(feature = "frame", target_os = "linux"))]
    let _orientation: Option<orientation::Guard> = if let Some(power) = &_display_power {
        let power = Rc::downgrade(power);
        orientation::start(&app, move |app, degrees| {
            let Some(power) = power.upgrade() else { return false; };
            if power.is_touching() { return false; }
            let frame = app.global::<Frame>();
            if frame.get_orientation_degrees() != degrees {
                frame.set_orientation_degrees(degrees);
                eprintln!("[orientation] UI rotation: {degrees} degrees");
                power.wake();
            }
            true
        })
    } else {
        eprintln!("[orientation] touch observer unavailable; fixed orientation");
        None
    };
    let _settings_save_guard = settings::save_guard();
    #[cfg(all(feature = "frame", target_os = "linux"))]
    shutdown::listen(termination_signals)?;
    app.run()?;
    Ok(())
}
