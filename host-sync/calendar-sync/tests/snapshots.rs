use calendar_sync::{merge, receive, Event, EventTime, Snapshot, Source, MAX_BYTES, READY, STORED};

fn meeting(id: &str, uid: Option<&str>, title: &str) -> Event {
    Event {
        id: id.into(),
        uid: uid.map(str::to_owned),
        title: title.into(),
        detail: String::new(),
        time: EventTime::Timed {
            start: "2026-11-02T18:00:00Z".parse().unwrap(),
            end: "2026-11-02T19:00:00Z".parse().unwrap(),
        },
    }
}
fn snapshot(id: &str, events: Vec<Event>) -> Snapshot {
    Snapshot::new(vec![Source {
        id: id.into(),
        events,
    }])
}

#[test]
fn overlapping_providers_dedupe_with_direct_fields_preferred() {
    let direct = snapshot("google", vec![meeting("g1", Some("uid1"), "Planning")]);
    let host = snapshot(
        "work",
        vec![meeting("w1", Some("uid1"), "Renamed planning")],
    );
    let mut host = host;
    if let EventTime::Timed { end, .. } = &mut host.sources[0].events[0].time {
        *end += chrono::Duration::minutes(15);
    }
    let result = merge([&direct, &host]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].title, "Planning");
    assert_eq!(merge([&host]).len(), 1);
    let replay = merge([&direct, &host, &host]);
    assert_eq!(replay, result);
}

#[test]
fn fallback_uses_normalized_title_and_exact_times_not_distinct_known_uids() {
    let direct = snapshot("google", vec![meeting("g1", None, "  Team   SYNC ")]);
    let host = snapshot("work", vec![meeting("w1", Some("uid1"), "team sync")]);
    assert_eq!(merge([&direct, &host]).len(), 1);
    let distinct = snapshot("other", vec![meeting("w2", Some("uid2"), "team sync")]);
    assert_eq!(merge([&direct, &host, &distinct]).len(), 2);
    let same_source_distinct = snapshot(
        "work",
        vec![meeting("one", None, "Sync"), meeting("two", None, "Sync")],
    );
    assert_eq!(merge([&same_source_distinct]).len(), 2);
    let mut different_end = meeting("diff", None, "team sync");
    if let EventTime::Timed { end, .. } = &mut different_end.time {
        *end += chrono::Duration::minutes(1);
    }
    assert_eq!(
        merge([&host, &snapshot("other", vec![different_end])]).len(),
        2
    );
}

#[test]
fn adding_a_provider_does_not_collapse_distinct_same_source_meetings() {
    let direct = snapshot("google", vec![meeting("g", None, "Sync")]);
    let host = snapshot(
        "work",
        vec![meeting("w1", None, "Sync"), meeting("w2", None, "Sync")],
    );
    assert_eq!(merge([&host]).len(), 2);
    assert_eq!(merge([&direct, &host]).len(), 2);
    assert_eq!(merge([&host, &direct]).len(), 2);
}

#[test]
fn suppressed_record_aliases_remain_matchable_without_changing_display_precedence() {
    let direct = snapshot("google", vec![meeting("g", Some("uid"), "Planning")]);
    let mut renamed = meeting("w", Some("uid"), "Renamed planning");
    if let EventTime::Timed { end, .. } = &mut renamed.time {
        *end += chrono::Duration::minutes(15);
    }
    let host = snapshot("work", vec![renamed.clone()]);
    let third = snapshot(
        "other",
        vec![Event {
            id: "o".into(),
            uid: None,
            ..renamed
        }],
    );
    for sources in [
        [&direct, &host, &third],
        [&direct, &third, &host],
        [&host, &direct, &third],
        [&host, &third, &direct],
        [&third, &direct, &host],
        [&third, &host, &direct],
    ] {
        let merged = merge(sources);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].title, sources[0].sources[0].events[0].title);
        assert_eq!(merged[0].time, sources[0].sources[0].events[0].time);
    }
}

#[test]
fn recurring_occurrences_and_dates_are_not_collapsed() {
    let first = meeting("one", Some("series"), "Recurring");
    let mut second = first.clone();
    second.id = "two".into();
    if let EventTime::Timed { start, end } = &mut second.time {
        *start += chrono::Duration::days(1);
        *end += chrono::Duration::days(1);
    }
    let direct = snapshot("google", vec![first.clone(), second.clone()]);
    let host = snapshot("work", vec![first, second]);
    assert_eq!(merge([&direct, &host]).len(), 2);
}

#[test]
fn invalid_boundary_data_is_rejected() {
    let valid = snapshot("work", vec![meeting("id", None, "Meeting")]);
    assert_eq!(Snapshot::decode(&valid.encode().unwrap()).unwrap(), valid);
    let mut bad = valid.clone();
    bad.version = 2;
    assert!(bad.encode().is_err());
    bad = valid.clone();
    bad.sources[0].events[0].title.clear();
    assert!(bad.encode().is_err());
    bad = valid.clone();
    if let EventTime::Timed { start, end } = &mut bad.sources[0].events[0].time {
        *end = *start;
    }
    assert!(bad.encode().is_err());
    bad = valid.clone();
    let duplicate = bad.sources[0].events[0].clone();
    bad.sources[0].events.push(duplicate);
    assert!(bad.encode().is_err());
    bad = valid.clone();
    let duplicate = bad.sources[0].clone();
    bad.sources.push(duplicate);
    assert!(bad.encode().is_err());
    assert!(Snapshot::decode(&vec![b' '; MAX_BYTES + 1]).is_err());
    let raw = String::from_utf8(valid.encode().unwrap())
        .unwrap()
        .replace("2026-11-02T18:00:00Z", "2026-11-02T18:00:00");
    assert!(Snapshot::decode(raw.as_bytes()).is_err());
}

#[test]
fn wire_ack_follows_persistence_and_invalid_updates_leave_last_good_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("calendar.json");
    let first = snapshot("work", vec![meeting("id", None, "Meeting")]);
    let mut wire = first.encode().unwrap();
    wire.push(b'\n');
    let mut output = Vec::new();
    receive(&wire[..], &mut output, &target).unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        format!("{READY}\n{STORED}\n")
    );
    assert_eq!(Snapshot::load(&target).unwrap(), first);
    let before = std::fs::read(&target).unwrap();
    for wire in [
        b"{}\n".to_vec(),
        first.encode().unwrap(),
        vec![b'x'; MAX_BYTES + 2],
    ] {
        let mut output = Vec::new();
        assert!(receive(&wire[..], &mut output, &target).is_err());
        assert!(!String::from_utf8(output).unwrap().contains(STORED));
        assert_eq!(std::fs::read(&target).unwrap(), before);
    }
    let empty = snapshot("work", vec![]);
    let mut wire = empty.encode().unwrap();
    wire.push(b'\n');
    receive(&wire[..], Vec::new(), &target).unwrap();
    assert!(Snapshot::load(&target).unwrap().sources[0]
        .events
        .is_empty());
    receive(&wire[..], Vec::new(), &target).unwrap();
    assert_eq!(Snapshot::load(&target).unwrap(), empty);
}
