use super::{trace, Buffer, ThumbStore, ThumbnailKey};
use frame_trace::Field;
use std::collections::HashSet;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

// Includes the buffer being encoded. Cache writes may be dropped under pressure.
const MAX_BYTES: usize = 24 * 1024 * 1024;
const MAX_ITEMS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Destination {
    Full,
    Thumb,
}

pub(super) struct PendingWrite {
    pub trace: Option<trace::Context>,
    pub key: ThumbnailKey,
    pub buffer: Buffer,
}

impl PendingWrite {
    pub fn save(self, store: &ThumbStore, destination: Destination) {
        if ThumbnailKey::read(&self.key.path, self.key.size, self.key.baked)
            .ok()
            .as_ref()
            != Some(&self.key)
        {
            trace::emit(
                self.trace,
                "photo.store.cancel",
                &[Field::Str("reason", "source_changed")],
            );
            return;
        }
        let (name, result) = match destination {
            Destination::Full => {
                let name = self.key.stored_full_name();
                let result = store.save_full(&name, &self.buffer, self.trace);
                (name, result)
            }
            Destination::Thumb => {
                let name = self.key.stored_name();
                let result = store.save(&name, &self.buffer, self.trace);
                (name, result)
            }
        };
        if let Err(error) = result {
            eprintln!("[photos] cannot store {destination:?} image {name}: {error}");
        }
    }
}

pub(super) struct CacheStore {
    pub disk: Arc<ThumbStore>,
    pub writer: CacheWriter,
}

impl CacheStore {
    pub fn from_env() -> Option<Self> {
        let disk = Arc::new(ThumbStore::from_env()?);
        let writer_disk = Arc::clone(&disk);
        let writer = CacheWriter::new(MAX_BYTES, MAX_ITEMS, move |pending| {
            pending.save(&writer_disk, Destination::Full);
        });
        Some(Self { disk, writer })
    }
}

struct State {
    bytes: usize,
    names: HashSet<String>,
}

struct Item {
    pending: PendingWrite,
    name: String,
    bytes: usize,
    queued: Option<Instant>,
}

pub(super) struct CacheWriter {
    sender: mpsc::Sender<Item>,
    state: Arc<Mutex<State>>,
    max_bytes: usize,
    max_items: usize,
}

impl CacheWriter {
    pub(super) fn new(
        max_bytes: usize,
        max_items: usize,
        write: impl Fn(PendingWrite) + Send + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::channel::<Item>();
        let state = Arc::new(Mutex::new(State {
            bytes: 0,
            names: HashSet::new(),
        }));
        let worker_state = Arc::clone(&state);
        std::thread::spawn(move || {
            while let Ok(item) = receiver.recv() {
                let context = item.pending.trace;
                trace::duration(context, "photo.store.queue_wait", item.queued, &[]);
                let started = trace::start(context);
                write(item.pending);
                trace::duration(context, "photo.store.writer_end", started, &[]);
                let mut state = worker_state.lock().unwrap();
                state.bytes -= item.bytes;
                state.names.remove(&item.name);
            }
        });
        Self {
            sender,
            state,
            max_bytes,
            max_items,
        }
    }

    pub(super) fn enqueue(&self, pending: PendingWrite) {
        let context = pending.trace;
        let name = pending.key.stored_full_name();
        let bytes = pending.buffer.as_bytes().len();
        let mut state = self.state.lock().unwrap();
        let mut item = None;
        let result = if state.names.contains(&name) {
            "duplicate"
        } else if state.names.len() >= self.max_items
            || bytes > self.max_bytes.saturating_sub(state.bytes)
        {
            "full"
        } else {
            state.bytes += bytes;
            state.names.insert(name.clone());
            item = Some(Item {
                pending,
                name: name.clone(),
                bytes,
                queued: trace::start(context),
            });
            "admitted"
        };
        trace::emit(
            context,
            "photo.store.queue",
            &[
                Field::Str("result", result),
                Field::U64("bytes", bytes as u64),
                Field::U64("reserved_bytes", state.bytes as u64),
            ],
        );
        if let Some(item) = item {
            if self.sender.send(item).is_err() {
                state.bytes -= bytes;
                state.names.remove(&name);
                eprintln!("[photos] cache writer disconnected; skipping persistence");
                trace::emit(
                    context,
                    "photo.store.cancel",
                    &[Field::Str("reason", "writer_disconnected")],
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::photos::{Buffer, ThumbnailKey};
    use std::time::{Duration, SystemTime};

    fn pending(name: &str) -> PendingWrite {
        PendingWrite {
            trace: None,
            key: ThumbnailKey {
                path: name.into(),
                modified: SystemTime::UNIX_EPOCH,
                length: 1,
                size: (10, 10, 0),
                baked: false,
            },
            buffer: Buffer::new(10, 10),
        }
    }

    #[test]
    fn blocked_writer_bounds_active_and_queued_buffers_without_blocking_admission() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = CacheWriter::new(600, 8, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        writer.enqueue(pending("one"));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        writer.enqueue(pending("one"));
        writer.enqueue(pending("two"));
        writer.enqueue(pending("three"));
        {
            let state = writer.state.lock().unwrap();
            assert_eq!(state.bytes, 600);
            assert_eq!(state.names.len(), 2);
        }
        release_tx.send(()).unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        release_tx.send(()).unwrap();
    }
}
