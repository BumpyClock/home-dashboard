use chrono::{Local, Timelike};
use slint::{Model, ModelRc, SharedString, VecModel};
use std::time::Duration;

use crate::{AppWindow, Frame, HourSlot};
use slint::ComponentHandle;

pub fn city() -> String {
    std::env::var("FRAME_CITY").unwrap_or_else(|_| "Seattle".into())
}

fn wmo(code: i64, day: bool) -> (&'static str, &'static str) {
    match code {
        0 => ("Clear", if day { "sun" } else { "moon" }),
        1 => ("Mostly clear", if day { "sun" } else { "moon" }),
        2 => ("Partly cloudy", if day { "cloud-sun" } else { "cloud-moon" }),
        3 => ("Cloudy", "cloud"),
        45 | 48 => ("Fog", "cloud-fog"),
        51..=57 => ("Drizzle", "cloud-drizzle"),
        61..=67 | 80..=82 => ("Rain", "cloud-rain"),
        71..=77 | 85 | 86 => ("Snow", "cloud-snow"),
        95..=99 => ("Storms", "cloud-lightning"),
        _ => ("—", "cloud"),
    }
}

fn wet(code: i64) -> bool { code >= 51 }

fn hour_label(h: usize) -> String {
    let h = h % 24;
    let n = if h % 12 == 0 { 12 } else { h % 12 };
    format!("{n} {}", if h < 12 { "am" } else { "pm" })
}

fn temperature(value: i64) -> String { format!("{value}°") }

fn weather_line(city: &str, hi: i64, lo: i64) -> String {
    format!("{city} · H {} L {}", temperature(hi), temperature(lo))
}

const MAX_WEATHER_BYTES: u64 = 64 * 1024;

const SLOT_STRIDE_H: usize = 3;
const SLOT_COUNT: usize = 5;

fn next_slot_boundary_h(now_h: usize) -> usize { (now_h / SLOT_STRIDE_H + 1) * SLOT_STRIDE_H }

fn read_weather(body: &mut ureq::Body) -> Result<serde_json::Value, String> {
    let text = body.with_config().limit(MAX_WEATHER_BYTES).read_to_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

fn hourly_matches(current: &ModelRc<HourSlot>, slots: &[HourSlot]) -> bool {
    current.row_count() == slots.len() && slots.iter().enumerate().all(|(i, next)| {
        current.row_data(i).is_some_and(|old| old.label == next.label && old.icon == next.icon && old.temp == next.temp)
    })
}

pub fn spawn(weak: slint::Weak<AppWindow>) {
    std::thread::spawn(move || {
        let lat = std::env::var("FRAME_LAT").unwrap_or_else(|_| "47.6062".into());
        let lon = std::env::var("FRAME_LON").unwrap_or_else(|_| "-122.3321".into());
        let city = city();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20))).build().into();
        let url = format!(
            "https://api.open-meteo.com/v1/forecast?latitude={lat}&longitude={lon}\
             &current=temperature_2m,weather_code,is_day\
             &hourly=temperature_2m,weather_code,is_day\
             &daily=temperature_2m_max,temperature_2m_min&forecast_days=2&timezone=auto"
        );
        loop {
            let parsed = agent.get(&url).call()
                .map_err(|e| e.to_string())
                .and_then(|mut r| read_weather(r.body_mut()));
            match parsed {
                Ok(v) => {
                    let num = |x: &serde_json::Value| x.as_f64().map(|f| f.round() as i64);
                    let temp = num(&v["current"]["temperature_2m"]).unwrap_or(0);
                    let code = v["current"]["weather_code"].as_i64().unwrap_or(3);
                    let is_day = v["current"]["is_day"].as_i64().unwrap_or(1) == 1;
                    let (cond, icon) = wmo(code, is_day);
                    let hi = num(&v["daily"]["temperature_2m_max"][0]).unwrap_or(0);
                    let lo = num(&v["daily"]["temperature_2m_min"][0]).unwrap_or(0);
                    let now_h = Local::now().hour() as usize;
                    let hourly = &v["hourly"];
                    let code_at = |i: usize| hourly["weather_code"][i].as_i64();

                    let ahead = (now_h + 1)..(now_h + 13);
                    let sub = if wet(code) {
                        match ahead.clone().find(|&i| code_at(i).is_some_and(|c| !wet(c))) {
                            Some(i) => format!("easing by {}", hour_label(i)),
                            None => "for the next while".into(),
                        }
                    } else {
                        match ahead.clone().find(|&i| code_at(i).is_some_and(wet)) {
                            Some(i) => format!("{} after {}", wmo(code_at(i).unwrap(), true).0.to_lowercase(), hour_label(i)),
                            None => "dry for the next 12 h".into(),
                        }
                    };

                    let first = next_slot_boundary_h(now_h);
                    let slots: Vec<HourSlot> = (0..SLOT_COUNT).filter_map(|k| {
                        let i = first + k * SLOT_STRIDE_H;
                        let t = num(&hourly["temperature_2m"][i])?;
                        let day = hourly["is_day"][i].as_i64().unwrap_or(1) == 1;
                        let (_, ic) = wmo(code_at(i)?, day);
                        Some(HourSlot { label: hour_label(i).into(), icon: ic.into(), temp: temperature(t).into() })
                    }).collect();

                    let w = weak.clone();
                    let line = SharedString::from(weather_line(&city, hi, lo));
                    let _ = slint::invoke_from_event_loop(move || {
                        let Some(app) = w.upgrade() else { return };
                        let f = app.global::<Frame>();
                        let temp = SharedString::from(temperature(temp));
                        let cond = SharedString::from(cond);
                        let sub = SharedString::from(sub);
                        let icon = SharedString::from(icon);
                        if f.get_weather_line() != line { f.set_weather_line(line); }
                        if f.get_temp() != temp { f.set_temp(temp); }
                        if f.get_cond() != cond { f.set_cond(cond); }
                        if f.get_cond_sub() != sub { f.set_cond_sub(sub); }
                        if f.get_weather_icon() != icon { f.set_weather_icon(icon); }
                        if !slots.is_empty() && !hourly_matches(&f.get_hourly(), &slots) {
                            f.set_hourly(ModelRc::new(VecModel::from(slots)));
                        }
                    });
                }
                Err(e) => eprintln!("[weather] fetch failed, keeping previous weather: {e}"),
            }
            std::thread::sleep(Duration::from_secs(15 * 60));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{hourly_matches, read_weather, temperature, weather_line, wmo, MAX_WEATHER_BYTES};
    use crate::HourSlot;
    use slint::{ModelRc, VecModel};

    #[test]
    fn weather_text_uses_correct_unicode_symbols() {
        assert_eq!(temperature(-4), "-4\u{00b0}");
        assert_eq!(weather_line("Seattle", 27, 19), "Seattle \u{00b7} H 27\u{00b0} L 19\u{00b0}");
        assert_eq!(wmo(-1, true).0, "\u{2014}");
    }

    #[test]
    fn response_cap_rejects_oversized_data_and_accepts_valid_json() {
        let mut valid = ureq::Body::builder().data(br#"{"current":{"temperature_2m":23}}"#.to_vec());
        assert_eq!(read_weather(&mut valid).unwrap()["current"]["temperature_2m"], 23);
        let below_limit = format!(r#"{{"pad":"{}"}}"#, " ".repeat(MAX_WEATHER_BYTES as usize - 11));
        assert_eq!(below_limit.len(), MAX_WEATHER_BYTES as usize - 1);
        let mut bounded = ureq::Body::builder().data(below_limit);
        assert!(read_weather(&mut bounded).is_ok());
        let too_large = format!(r#"{{"pad":"{}"}}"#, " ".repeat(MAX_WEATHER_BYTES as usize - 9));
        let mut oversized = ureq::Body::builder().data(too_large);
        assert!(read_weather(&mut oversized).is_err());
    }

    #[test]
    fn hourly_comparison_covers_every_field_and_row_count() {
        let slot = HourSlot { label: "9 am".into(), icon: "sun".into(), temp: "23°".into() };
        let model = ModelRc::new(VecModel::from(vec![slot.clone()]));
        assert!(hourly_matches(&model, &[slot.clone()]));
        assert!(!hourly_matches(&model, &[]));
        for changed in [
            HourSlot { label: "12 pm".into(), ..slot.clone() },
            HourSlot { icon: "rain".into(), ..slot.clone() },
            HourSlot { temp: "24°".into(), ..slot },
        ] {
            assert!(!hourly_matches(&model, &[changed]));
        }
    }
}
