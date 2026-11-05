use super::cache_writer::{Destination, PendingWrite};
use super::{trace, Kind, PhotoSize, RenderResult, Rendered};

pub(super) fn complete<A: FnOnce()>(
    rendered: RenderResult,
    enqueue: impl FnOnce(Vec<(Kind, PhotoSize, Rendered)>) -> Result<A, ()>,
    mut persist: impl FnMut(Destination, PendingWrite),
    release: impl FnOnce(),
    context: Option<trace::Context>,
) -> bool {
    let Ok(wait_for_publication) = enqueue(rendered.outcomes) else {
        release();
        return false;
    };
    if let Some(pending) = rendered.thumb {
        persist(Destination::Thumb, pending);
    }
    let started = trace::start(context);
    wait_for_publication();
    trace::duration(context, "photo.ui.ack_wait", started, &[]);
    if let Some(pending) = rendered.full {
        persist(Destination::Full, pending);
    }
    release();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::photos::{Buffer, Shared, ThumbStore, ThumbnailKey};
    use slint::{Image, Model, VecModel};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(10);

    fn rendered(source: &std::path::Path) -> RenderResult {
        let key = ThumbnailKey::read(source, (2, 2, 0), false).unwrap();
        let mut buffer = Buffer::new(2, 2);
        buffer.make_mut_bytes().fill(42);
        let pending = || PendingWrite {
            trace: None,
            key: key.clone(),
            buffer: buffer.clone(),
        };
        RenderResult {
            outcomes: vec![(Kind::Thumb, (2, 2, 0), Rendered::Ready(buffer.clone()))],
            full: Some(pending()),
            thumb: Some(pending()),
        }
    }

    fn claimed() -> Arc<Mutex<Shared>> {
        let mut shared = Shared::new();
        shared.want.thumbs = 0..1;
        assert_eq!(shared.claim(1).job.unwrap().index, 0);
        Arc::new(Mutex::new(shared))
    }

    fn completion_order(ui_first: bool, fail_write: bool) {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.jpg");
        std::fs::write(&source, b"source identity fixture").unwrap();
        let rendered = rendered(&source);
        let cache = directory.path().join("cache");
        if fail_write {
            std::fs::write(&cache, b"not a directory").unwrap();
        }
        let store = ThumbStore::new(cache);
        let shared = claimed();
        let worker_shared = Arc::clone(&shared);
        let (queued, ui) = mpsc::sync_channel(1);
        let (started, writing) = mpsc::sync_channel(1);
        let (allow_write, may_write) = mpsc::sync_channel(1);
        let (saved, write_done) = mpsc::sync_channel(1);
        let (waiting, awaiting_publication) = mpsc::sync_channel(1);
        let (released, done) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            complete(
                rendered,
                |outcomes| {
                    let (acknowledge, acknowledged) = mpsc::sync_channel::<()>(1);
                    queued.send((outcomes, acknowledge)).unwrap();
                    Ok(move || {
                        waiting.send(()).unwrap();
                        assert_eq!(
                            acknowledged.recv_timeout(WAIT),
                            Err(mpsc::RecvTimeoutError::Disconnected)
                        );
                    })
                },
                |destination, pending| {
                    assert!(worker_shared.lock().unwrap().in_flight.contains(&0));
                    if destination == Destination::Thumb {
                        started.send(()).unwrap();
                        may_write.recv_timeout(WAIT).unwrap();
                    }
                    pending.save(&store, destination);
                    saved.send(destination).unwrap();
                },
                || {
                    worker_shared.lock().unwrap().release(0);
                    released.send(()).unwrap();
                },
                None,
            )
        });
        let (outcomes, acknowledgement) = ui.recv_timeout(WAIT).unwrap();
        writing.recv_timeout(WAIT).unwrap();
        let model = VecModel::from(vec![Image::default()]);
        let Rendered::Ready(pixels) = outcomes.into_iter().next().unwrap().2 else {
            panic!("display pixels were lost");
        };
        assert_eq!(pixels.as_bytes(), &[42; 12]);
        model.set_row_data(0, Image::from_rgb8(pixels));
        let size = model.row_data(0).unwrap().size();
        assert_eq!((size.width, size.height), (2, 2));
        assert!(shared.lock().unwrap().in_flight.contains(&0));
        assert!(done.try_recv().is_err());
        if ui_first {
            drop(acknowledgement);
            assert!(shared.lock().unwrap().in_flight.contains(&0));
            assert!(done.try_recv().is_err());
            allow_write.send(()).unwrap();
            assert_eq!(write_done.recv_timeout(WAIT).unwrap(), Destination::Thumb);
        } else {
            allow_write.send(()).unwrap();
            assert_eq!(write_done.recv_timeout(WAIT).unwrap(), Destination::Thumb);
            awaiting_publication.recv_timeout(WAIT).unwrap();
            assert!(shared.lock().unwrap().in_flight.contains(&0));
            assert!(done.try_recv().is_err());
            assert!(
                write_done.try_recv().is_err(),
                "Full write preceded UI acknowledgement"
            );
            drop(acknowledgement);
        }
        assert_eq!(write_done.recv_timeout(WAIT).unwrap(), Destination::Full);
        done.recv_timeout(WAIT).unwrap();
        assert!(worker.join().unwrap());
        assert!(!shared.lock().unwrap().in_flight.contains(&0));
        let size = model.row_data(0).unwrap().size();
        assert_eq!((size.width, size.height), (2, 2));
    }

    #[test]
    fn publication_and_persistence_both_finish_before_claim_release() {
        completion_order(true, false);
        completion_order(false, false);
    }

    #[test]
    fn optional_write_failure_preserves_publication_and_releases_claim() {
        completion_order(true, true);
        completion_order(false, true);
    }

    #[test]
    fn abandoned_callback_finishes_barrier_without_publication_and_releases_claim() {
        let source = tempfile::NamedTempFile::new().unwrap();
        let rendered = rendered(source.path());
        let shared = claimed();
        let worker_shared = Arc::clone(&shared);
        let (queued, ui) = mpsc::sync_channel(1);
        let (waiting, awaiting_callback) = mpsc::sync_channel(1);
        let (saved, writes) = mpsc::sync_channel(2);
        let (released, done) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            complete(
                rendered,
                |outcomes| {
                    let (acknowledge, acknowledged) = mpsc::sync_channel::<()>(1);
                    queued.send((outcomes, acknowledge)).unwrap();
                    Ok(move || {
                        waiting.send(()).unwrap();
                        assert_eq!(
                            acknowledged.recv_timeout(WAIT),
                            Err(mpsc::RecvTimeoutError::Disconnected)
                        );
                    })
                },
                |destination, _| {
                    assert!(worker_shared.lock().unwrap().in_flight.contains(&0));
                    saved.send(destination).unwrap();
                },
                || {
                    worker_shared.lock().unwrap().release(0);
                    released.send(()).unwrap();
                },
                None,
            )
        });
        let callback = ui.recv_timeout(WAIT).unwrap();
        awaiting_callback.recv_timeout(WAIT).unwrap();
        assert_eq!(writes.recv_timeout(WAIT).unwrap(), Destination::Thumb);
        assert!(writes.try_recv().is_err());
        assert!(done.try_recv().is_err());
        assert!(shared.lock().unwrap().in_flight.contains(&0));
        drop(callback);
        assert_eq!(writes.recv_timeout(WAIT).unwrap(), Destination::Full);
        done.recv_timeout(WAIT).unwrap();
        assert!(worker.join().unwrap());
        assert!(!shared.lock().unwrap().in_flight.contains(&0));
    }

    #[test]
    fn rejected_enqueue_skips_all_persistence_and_releases_claim() {
        let source = tempfile::NamedTempFile::new().unwrap();
        let shared = claimed();
        assert!(!complete(
            rendered(source.path()),
            |_| Err::<fn(), ()>(()),
            |_, _| panic!("persistence ran after enqueue rejection"),
            || shared.lock().unwrap().release(0),
            None,
        ));
        assert!(!shared.lock().unwrap().in_flight.contains(&0));
    }
}
