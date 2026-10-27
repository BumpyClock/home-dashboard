//! Photo-job context stays with the existing claim, render, publication, and stored write.
use frame_trace::Field;
use slint::ComponentHandle;
use std::cell::Cell;
use std::time::Instant;

#[derive(Clone, Copy)]
pub(super) struct Context {
    pub id: u64,
    index: usize,
    warm: bool,
    kind: &'static str,
}

impl Context {
    pub fn new(index: usize, warm: bool) -> Option<Self> {
        frame_trace::enabled().then(|| Self {
            id: frame_trace::next_id(),
            index,
            warm,
            kind: "job",
        })
    }

    pub fn rendition(self, kind: &'static str) -> Self {
        Self { kind, ..self }
    }
}

pub(super) fn start(context: Option<Context>) -> Option<Instant> {
    context
        .filter(|_| frame_trace::enabled())
        .map(|_| Instant::now())
}

pub(super) fn emit(context: Option<Context>, name: &'static str, fields: &[Field]) {
    emit_fields(context, name, fields.iter().copied());
}

fn emit_fields(
    context: Option<Context>,
    name: &'static str,
    fields: impl IntoIterator<Item = Field>,
) {
    let Some(context) = context else { return };
    if !frame_trace::enabled() {
        return;
    }
    frame_trace::event_fields(
        name,
        Some(context.id),
        [
            Field::U64("source_index", context.index as u64),
            Field::Bool("warm", context.warm),
            Field::Str("kind", context.kind),
        ]
        .into_iter()
        .chain(fields),
    );
}

pub(super) fn duration(
    context: Option<Context>,
    name: &'static str,
    started: Option<Instant>,
    fields: &[Field],
) {
    let Some(started) = started else { return };
    emit_fields(
        context,
        name,
        std::iter::once(Field::U64("elapsed_us", micros(started))).chain(fields.iter().copied()),
    );
}

pub(super) fn micros(started: Instant) -> u64 {
    started.elapsed().as_micros().min(u64::MAX as u128) as u64
}

pub(crate) fn install_ui(app: &crate::AppWindow) {
    let ids = [Cell::new(None), Cell::new(None)];
    app.global::<crate::Frame>().on_trace_photo_stage(
        move |kind, action, requested, current, previous, ready| {
            let index = usize::from(kind == 1);
            let surface = if index == 1 { "screensaver" } else { "home" };
            let name = match action {
                0 => "photo_requested",
                1 => "photo_readiness",
                2 => "photo_transition_start",
                3 | 6 => "photo_transition_cancel",
                4 => "photo_transition_snap",
                5 => "photo_transition_end",
                _ => return,
            };
            if action == 2 {
                ids[index].set(frame_trace::enabled().then(frame_trace::next_id));
            }
            let id = if matches!(action, 2 | 3 | 5 | 6) {
                ids[index].get()
            } else {
                None
            };
            if matches!(action, 3 | 5 | 6) && id.is_none() {
                return;
            }
            frame_trace::event(
                name,
                id,
                &[
                    Field::Str("surface", surface),
                    Field::I64("requested", requested.into()),
                    Field::I64("displayed", current.into()),
                    Field::I64("previous", previous.into()),
                    Field::Bool("ready", ready),
                ],
            );
            if matches!(action, 3 | 5 | 6) {
                ids[index].set(None);
            }
        },
    );
    app.global::<crate::Frame>()
        .on_trace_grid(|kind, a, b, c, d, ready| {
            let (name, fields) = match kind {
                0 => (
                    "grid_range",
                    [
                        Field::I64("wanted_first", a.into()),
                        Field::I64("wanted_end", b.into()),
                        Field::I64("visible_first", c.into()),
                        Field::I64("visible_end", d.into()),
                        Field::Bool("ready", false),
                    ],
                ),
                1 | 2 => (
                    if kind == 1 {
                        "grid_slot"
                    } else {
                        "grid_thumbnail_ready"
                    },
                    [
                        Field::I64("slot", a.into()),
                        Field::I64("photo_index", b.into()),
                        Field::I64("wanted_first", c.into()),
                        Field::I64("wanted_end", d.into()),
                        Field::Bool("ready", ready),
                    ],
                ),
                _ => return,
            };
            frame_trace::event(name, None, &fields);
        });
}
