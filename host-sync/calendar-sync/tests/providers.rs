#![cfg(all(feature = "host", unix))]

use calendar_sync::sources::{Config, SourceConfig};
use calendar_sync::EventTime;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

fn executable(dir: &std::path::Path, body: &str) -> String {
    let path = dir.join("provider");
    std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path.to_str().unwrap().into()
}

#[test]
fn workiq_cli_and_mcp_execute_real_processes_and_keep_event_identity() {
    let dir = tempfile::tempdir().unwrap();
    let event = json!({"id":"real-occurrence","uid":"shared-uid","title":"Work meeting","detail":"Office",
        "time":{"kind":"timed","start":"2026-11-02T10:00:00-08:00","end":"2026-11-02T11:00:00-08:00"}});
    let answer = json!({"complete":true,"events":[event]});
    let cli = executable(
        dir.path(),
        &format!("test \"$1\" = ask\ntest \"$2\" = -q\nprintf '%s' '{answer}'"),
    );
    let config = Config {
        days: 7,
        time_zone: Some(chrono_tz::UTC),
        sources: vec![SourceConfig::Workiq {
            id: "work".into(),
            executable: cli,
        }],
    };
    config.validate().unwrap();
    let result = config
        .fetch(dir.path(), "2026-11-02T12:00:00Z".parse().unwrap())
        .unwrap();
    assert_eq!(result.sources[0].events[0].id, "real-occurrence");
    assert_eq!(
        result.sources[0].events[0].uid.as_deref(),
        Some("shared-uid")
    );
    assert_eq!(
        result.sources[0].events[0].time,
        EventTime::Timed {
            start: "2026-11-02T18:00:00Z".parse().unwrap(),
            end: "2026-11-02T19:00:00Z".parse().unwrap()
        }
    );
    let envelope = json!({"content":[{"type":"text","text":json!({"answer":answer.to_string()}).to_string()}]});
    let mcp = executable(
        dir.path(),
        &format!("test \"$1\" = call\ntest \"$2\" = Workiq.ask\nprintf '%s' '{envelope}'"),
    );
    let config = Config {
        days: 7,
        time_zone: Some(chrono_tz::UTC),
        sources: vec![SourceConfig::WorkiqMcp {
            id: "work".into(),
            executable: mcp,
            server: "Workiq".into(),
        }],
    };
    assert_eq!(
        config
            .fetch(dir.path(), "2026-11-02T12:00:00Z".parse().unwrap())
            .unwrap()
            .sources,
        result.sources
    );
}

#[test]
fn calendar_mcp_expands_window_and_errors_do_not_become_empty_success() {
    let dir = tempfile::tempdir().unwrap();
    let response = include_str!("fixtures/calendar-view-mcp.json");
    let cmd = executable(dir.path(), &format!("test \"$1\" = call\ntest \"$2\" = MicrosoftCalendar.ListCalendarView\nprintf '%s' \"$4\" > '{}'\nprintf '%s' '{response}'", dir.path().join("args.json").display()));
    let config = Config {
        days: 7,
        time_zone: Some(chrono_tz::UTC),
        sources: vec![SourceConfig::McporterCalendar {
            id: "work".into(),
            executable: cmd,
            server: "MicrosoftCalendar".into(),
        }],
    };
    let result = config
        .fetch(dir.path(), "2026-11-02T12:00:00Z".parse().unwrap())
        .unwrap();
    assert_eq!(result.sources[0].events.len(), 2);
    assert_eq!(result.sources[0].events[0].title, "Calendar meeting");
    assert_eq!(
        result.sources[0].events[0].uid.as_deref(),
        Some("shared-uid")
    );
    assert_eq!(
        result.sources[0].events[1].time,
        EventTime::AllDay {
            start: "2026-11-03".parse().unwrap(),
            end: "2026-11-04".parse().unwrap(),
        }
    );
    let args: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("args.json")).unwrap()).unwrap();
    assert_eq!(args["startDateTime"], "2026-11-02T00:00:00+00:00");
    assert_eq!(args["endDateTime"], "2026-11-09T00:00:00+00:00");
    assert_eq!(args["timeZone"], "UTC");
    assert_eq!(args["orderby"], "start/dateTime");
    assert_eq!(
        args["select"],
        "id,iCalUId,subject,start,end,isAllDay,isCancelled,location"
    );
    executable(
        dir.path(),
        "printf '%s' '{\"isError\":true,\"content\":[]}'",
    );
    assert!(config.fetch(dir.path(), chrono::Utc::now()).is_err());
    executable(dir.path(), "exit 1");
    assert!(config.fetch(dir.path(), chrono::Utc::now()).is_err());
}

#[test]
fn calendar_window_starts_today_in_selected_zone_and_tracks_dst_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    for (now, zone, start, end) in [
        (
            "2026-11-02T01:00:00Z",
            chrono_tz::America::Los_Angeles,
            "2026-11-01T07:00:00+00:00",
            "2026-11-03T08:00:00+00:00",
        ),
        (
            "2026-11-02T23:00:00Z",
            chrono_tz::Asia::Tokyo,
            "2026-11-02T15:00:00+00:00",
            "2026-11-04T15:00:00+00:00",
        ),
    ] {
        let response = json!({"content":[{"type":"text","text":"Calendar view retrieved successfully.\n{\"value\":[]}"}]});
        let cmd = executable(
            dir.path(),
            &format!(
                "printf '%s' \"$4\" > '{}'\nprintf '%s' '{response}'",
                dir.path().join("args.json").display()
            ),
        );
        let config = Config {
            days: 2,
            time_zone: Some(zone),
            sources: vec![SourceConfig::McporterCalendar {
                id: "work".into(),
                executable: cmd,
                server: "MicrosoftCalendar".into(),
            }],
        };
        assert!(config
            .fetch(dir.path(), now.parse().unwrap())
            .unwrap()
            .sources[0]
            .events
            .is_empty());
        let args: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("args.json")).unwrap()).unwrap();
        assert_eq!(args["startDateTime"], start);
        assert_eq!(args["endDateTime"], end);
    }
    assert!(
        serde_json::from_str::<Config>(r#"{"days":7,"time_zone":"Not/AZone","sources":[]}"#)
            .is_err()
    );
}

#[test]
fn calendar_mcp_accepts_empty_payloads_but_not_errors_or_ambiguous_data() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        days: 7,
        time_zone: Some(chrono_tz::UTC),
        sources: vec![SourceConfig::McporterCalendar {
            id: "work".into(),
            executable: dir.path().join("provider").to_str().unwrap().into(),
            server: "MicrosoftCalendar".into(),
        }],
    };
    for response in [
        json!({"content":[{"type":"text","text":"CorrelationId: fixture, TimeStamp: fixture"},
            {"type":"text","text":"Calendar view retrieved successfully.\r\n{\"value\":[],\"@odata.nextLink\":null}"}]}),
        json!({"structuredContent":{"value":[]},"content":[{"type":"text","text":"extra status"}]}),
        json!({"structuredContent":null,"content":[{"type":"text","text":"{\"value\":[]}"}]}),
        json!({"value":[]}),
    ] {
        executable(dir.path(), &format!("printf '%s' '{response}'"));
        let result = config.fetch(dir.path(), chrono::Utc::now()).unwrap();
        assert!(result.sources[0].events.is_empty());
    }
    for response in [
        json!({"isError":true,"structuredContent":{"value":[]}}),
        json!({"structuredContent":{"error":{"code":"AccessDenied"}}}),
        json!({"content":[{"type":"text","text":"CorrelationId: fixture"}]}),
        json!({"content":[{"type":"text","text":"Calendar view retrieved successfully.\n{\"value\":["}]}),
        json!({"content":[{"type":"text","text":"Unknown preface.\n{\"value\":[]}"}]}),
        json!({"content":[{"type":"text","text":"{\"value\":[]}"},{"type":"text","text":"Results may be incomplete"}]}),
        json!({"content":[{"type":"text","text":"{\"value\":[]}"},{"type":"text","text":"{\"value\":[]}"}]}),
        json!({"value":[],"@odata.nextLink":"more"}),
        json!({"value":[],"hasMore":true}),
        json!({"message":"No access"}),
    ] {
        executable(dir.path(), &format!("printf '%s' '{response}'"));
        assert!(
            config.fetch(dir.path(), chrono::Utc::now()).is_err(),
            "accepted incomplete or malformed response"
        );
    }
}
