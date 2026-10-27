//! Bounded, opt-in local trace. Event producers never write files or serialize JSON.
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::OnceLock;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const CAPACITY: usize = 8192;
const MAX_FIELDS: usize = 12;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static DROPPED: AtomicU64 = AtomicU64::new(0);
static TRUNCATED: AtomicU64 = AtomicU64::new(0);
static SESSION: OnceLock<Session> = OnceLock::new();

#[derive(Clone, Copy)]
pub enum Field {
    U64(&'static str, u64),
    I64(&'static str, i64),
    Bool(&'static str, bool),
    Str(&'static str, &'static str),
}
struct Event {
    at_us: u64,
    name: &'static str,
    op_id: Option<u64>,
    fields: [Option<Field>; MAX_FIELDS],
}
#[expect(
    clippy::large_enum_variant,
    reason = "Inline events keep the bounded producer path allocation-free."
)]
enum Message {
    Event(Event),
    Finish,
}
struct Session {
    origin: Instant,
    sender: SyncSender<Message>,
}
pub struct Guard {
    worker: Option<JoinHandle<()>>,
}

pub fn enabled() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

thread_local! { static CURRENT_FRAME: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) }; }

/// Assign one correlation ID to the frame on the rendering thread.
pub fn begin_frame() -> Option<u64> {
    if !enabled() {
        return None;
    }
    let id = next_id();
    CURRENT_FRAME.with(|frame| frame.set(Some(id)));
    Some(id)
}

/// Read the current rendering thread's frame ID, if capture is active.
pub fn current_frame() -> Option<u64> {
    if !enabled() {
        return None;
    }
    CURRENT_FRAME.with(std::cell::Cell::get)
}

/// Clear the rendering thread's frame ID after submission.
pub fn end_frame() {
    CURRENT_FRAME.with(|frame| frame.set(None));
}

pub fn event(name: &'static str, op_id: Option<u64>, fields: &[Field]) {
    event_fields(name, op_id, fields.iter().copied());
}

/// Pack composed field sets without allocating or hiding truncation.
pub fn event_fields(
    name: &'static str,
    op_id: Option<u64>,
    fields: impl IntoIterator<Item = Field>,
) {
    if !enabled() {
        return;
    }
    let Some(session) = SESSION.get() else {
        return;
    };
    let mut packed = [None; MAX_FIELDS];
    for (index, field) in fields.into_iter().enumerate() {
        if index == MAX_FIELDS {
            TRUNCATED.fetch_add(1, Ordering::Relaxed);
            break;
        }
        packed[index] = Some(field);
    }
    let event = Event {
        at_us: session.origin.elapsed().as_micros().min(u64::MAX as u128) as u64,
        name,
        op_id,
        fields: packed,
    };
    match session.sender.try_send(Message::Event(event)) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            DROPPED.fetch_add(1, Ordering::Relaxed);
        }
        Err(TrySendError::Disconnected(_)) => {
            ACTIVE.store(false, Ordering::Relaxed);
        }
    }
}

/// Opens a new absolute output path only when explicitly configured. Errors are nonfatal.
pub fn init_from_env() -> Guard {
    let Some(path) = std::env::var_os("FRAME_TRACE_OUTPUT") else {
        return Guard { worker: None };
    };
    let path = Path::new(&path);
    if !path.is_absolute() {
        eprintln!("[trace] FRAME_TRACE_OUTPUT must be absolute");
        return Guard { worker: None };
    }
    let file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("[trace] cannot open output: {error}");
            return Guard { worker: None };
        }
    };
    let seconds = std::env::var("FRAME_TRACE_SECONDS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(1, 180);
    let (sender, receiver) = mpsc::sync_channel(CAPACITY);
    let origin = Instant::now();
    if SESSION.set(Session { origin, sender }).is_err() {
        eprintln!("[trace] already initialized");
        return Guard { worker: None };
    }
    ACTIVE.store(true, Ordering::Release);
    let worker = thread::spawn(move || {
        let mut out = BufWriter::new(file);
        let mut written = 0u64;
        let mut failed = false;
        let mut counts = std::collections::BTreeMap::<&'static str, u64>::new();
        let deadline = origin + Duration::from_secs(seconds);
        let mut next_sample = origin;
        #[cfg(target_os = "linux")]
        let ticks_per_second = {
            let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            (ticks > 0).then_some(ticks)
        };
        #[cfg(not(target_os = "linux"))]
        let ticks_per_second: Option<i64> = None;
        let header = serde_json::json!({"schema":1,"type":"session","clock":"monotonic_us","requested_seconds":seconds,"cpu_ticks_per_second":ticks_per_second,"render_hooks_compiled":cfg!(feature = "render-hooks"),"gpu_completion_available":null,"gpu_completion_timestamp_available":false});
        if serde_json::to_writer(&mut out, &header).is_err()
            || writeln!(out).is_err()
            || out.flush().is_err()
        {
            failed = true;
        }
        loop {
            let now = Instant::now();
            if failed || now >= deadline {
                break;
            }
            if now >= next_sample {
                sample_resources(&mut out, origin, &mut failed);
                if out.flush().is_err() {
                    failed = true;
                }
                if failed {
                    break;
                }
                next_sample = now + Duration::from_secs(1);
            }
            match receiver.recv_timeout(
                deadline
                    .min(next_sample)
                    .saturating_duration_since(Instant::now()),
            ) {
                Ok(Message::Event(event)) => {
                    if write_event(&mut out, &event).is_err() {
                        failed = true;
                        break;
                    }
                    *counts.entry(event.name).or_default() += 1;
                    written += 1;
                }
                Ok(Message::Finish) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        ACTIVE.store(false, Ordering::Release);
        while !failed {
            let Ok(Message::Event(event)) = receiver.try_recv() else {
                break;
            };
            if write_event(&mut out, &event).is_err() {
                failed = true;
                break;
            }
            *counts.entry(event.name).or_default() += 1;
            written += 1;
        }
        let summary = serde_json::json!({"type":"summary", "events":written,"dropped":DROPPED.load(Ordering::Relaxed),"truncated_fields":TRUNCATED.load(Ordering::Relaxed),"counts":counts,"duration_us":origin.elapsed().as_micros() as u64});
        if !failed
            && (serde_json::to_writer(&mut out, &summary).is_err()
                || writeln!(out).is_err()
                || out.flush().is_err())
        {
            failed = true;
        }
        if failed {
            eprintln!("[trace] output write failed");
        }
    });
    Guard {
        worker: Some(worker),
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            ACTIVE.store(false, Ordering::Release);
            if let Some(session) = SESSION.get() {
                let _ = session.sender.send(Message::Finish);
            }
            let _ = worker.join();
        }
    }
}
fn write_event(out: &mut impl Write, event: &Event) -> std::io::Result<()> {
    let mut fields = serde_json::Map::new();
    for field in event.fields.iter().flatten() {
        let (name, value) = match *field {
            Field::U64(n, v) => (n, serde_json::json!(v)),
            Field::I64(n, v) => (n, serde_json::json!(v)),
            Field::Bool(n, v) => (n, serde_json::json!(v)),
            Field::Str(n, v) => (n, serde_json::json!(v)),
        };
        fields.insert(name.into(), value);
    }
    serde_json::to_writer(
        &mut *out,
        &serde_json::json!({"type":"event","at_us":event.at_us,"name":event.name,"op_id":event.op_id,"fields":fields}),
    )?;
    writeln!(out)
}
fn sample_resources(out: &mut impl Write, origin: Instant, failed: &mut bool) {
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string("/proc/self/stat").ok();
        let parts: Vec<_> = stat
            .as_deref()
            .and_then(|s| s.rsplit_once(')'))
            .map(|(_, s)| s.split_whitespace().collect())
            .unwrap_or_default();
        let n = |i: usize| parts.get(i).and_then(|s| s.parse::<u64>().ok());
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let rss = status
            .lines()
            .find_map(|s| s.strip_prefix("VmRSS:"))
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<u64>().ok());
        let value = serde_json::json!({"type":"resource", "at_us":origin.elapsed().as_micros() as u64, "user_ticks":n(11), "system_ticks":n(12), "minor_faults":n(7), "rss_kib":rss});
        if !*failed && (serde_json::to_writer(&mut *out, &value).is_err() || writeln!(out).is_err())
        {
            *failed = true;
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (out, origin, failed);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_payload_has_numeric_ids_and_no_implicit_content() {
        let mut fields = [None; MAX_FIELDS];
        fields[0] = Some(Field::I64("index", -1));
        fields[1] = Some(Field::Bool("ready", true));
        let mut bytes = Vec::new();
        write_event(
            &mut bytes,
            &Event {
                at_us: 123,
                name: "transition",
                op_id: Some(4),
                fields,
            },
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["at_us"], 123);
        assert_eq!(value["op_id"], 4);
        assert_eq!(value["fields"]["index"], -1);
        assert_eq!(value["fields"]["ready"], true);
        assert_eq!(value["fields"].as_object().unwrap().len(), 2);
    }
    #[test]
    fn disabled_is_noop_and_ids_are_unique() {
        assert!(!enabled());
        event("ignored", None, &[]);
        assert_ne!(next_id(), next_id());
    }
}
