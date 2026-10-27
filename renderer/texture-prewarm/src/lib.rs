//! A bounded, UI-thread handoff registered by the patched FemtoVG renderer.

use i_slint_core::api::Image;

/// Queue the latest full-screen image for upload on the next render.
/// Returns false when renderer support is absent or the image is unsupported.
pub fn enqueue(image: Image) -> bool {
    clear();
    #[cfg(feature = "renderer")]
    {
        if !SUPPORTED.with(std::cell::Cell::get) {
            return false;
        }
        use i_slint_core::{graphics::SharedImageBuffer, ImageInner};
        let inner: &ImageInner = (&image).into();
        let ImageInner::EmbeddedImage {
            buffer: SharedImageBuffer::RGB8(buffer),
            ..
        } = inner
        else {
            return false;
        };
        let bytes = (buffer.width() as usize)
            .saturating_mul(buffer.height() as usize)
            .saturating_mul(4);
        if bytes == 0 || bytes > MAX_TEXTURE_BYTES {
            return false;
        }
        PENDING.with(|pending| *pending.borrow_mut() = Some((image, true)));
        true
    }
    #[cfg(not(feature = "renderer"))]
    {
        let _ = image;
        false
    }
}

/// Cancel an upload when the visible surface or its next rendition changes.
pub fn clear() {
    #[cfg(feature = "renderer")]
    PENDING.with(|pending| *pending.borrow_mut() = None);
}

/// Maximum estimated RGBA texture bytes retained by the renderer cache.
pub const MAX_TEXTURE_BYTES: usize = 16 * 1024 * 1024;

#[cfg(feature = "renderer")]
thread_local! {
    static SUPPORTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PENDING: std::cell::RefCell<Option<(Image, bool)>> = const { std::cell::RefCell::new(None) };
    static TRANSITION_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Called when the patched renderer creates its texture cache.
#[cfg(feature = "renderer")]
pub fn register_renderer() {
    SUPPORTED.with(|supported| supported.set(true));
}

/// Defer background texture uploads while a screensaver transition is active.
pub fn set_transition_active(active: bool) {
    #[cfg(feature = "renderer")]
    TRANSITION_ACTIVE.with(|value| value.set(active));
    #[cfg(not(feature = "renderer"))]
    let _ = active;
}

/// The renderer must request one more frame after `Deferred`, but not after `Idle`.
#[cfg(feature = "renderer")]
pub enum Poll {
    Idle,
    Deferred,
    Ready(Image),
}

/// Allow a normal frame to be presented before preparing the latest image.
#[cfg(feature = "renderer")]
pub fn poll() -> Poll {
    if TRANSITION_ACTIVE.with(|value| value.get()) {
        return Poll::Idle;
    }
    PENDING.with(|pending| {
        let mut pending = pending.borrow_mut();
        match pending.as_mut() {
            None => Poll::Idle,
            Some((_, defer)) if *defer => {
                *defer = false;
                Poll::Deferred
            }
            Some(_) => Poll::Ready(pending.take().unwrap().0),
        }
    })
}

#[cfg(all(test, feature = "renderer"))]
mod tests {
    use super::*;
    use i_slint_core::graphics::{Rgb8Pixel, SharedPixelBuffer};

    fn image(width: u32, height: u32) -> Image {
        Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::new(width, height))
    }

    #[test]
    fn queue_is_latest_only_and_defers_during_transitions() {
        register_renderer();
        set_transition_active(true);
        assert!(enqueue(image(2, 2)));
        let latest = image(3, 3);
        assert!(enqueue(latest.clone()));
        assert!(matches!(poll(), Poll::Idle));
        set_transition_active(false);
        assert!(matches!(poll(), Poll::Deferred));
        assert!(matches!(poll(), Poll::Ready(image) if image == latest));
        assert!(matches!(poll(), Poll::Idle));
    }

    #[test]
    fn replacement_gets_its_own_frame_and_animation_does_not_poll() {
        register_renderer();
        set_transition_active(false);
        assert!(enqueue(image(2, 2)));
        assert!(matches!(poll(), Poll::Deferred));
        let latest = image(3, 3);
        assert!(enqueue(latest.clone()));
        set_transition_active(true);
        assert!(matches!(poll(), Poll::Idle));
        assert!(matches!(poll(), Poll::Idle));
        set_transition_active(false);
        assert!(matches!(poll(), Poll::Deferred));
        set_transition_active(true);
        assert!(matches!(poll(), Poll::Idle));
        set_transition_active(false);
        assert!(matches!(poll(), Poll::Ready(image) if image == latest));
        assert!(matches!(poll(), Poll::Idle));
    }

    #[test]
    fn rejects_empty_and_oversized_images() {
        register_renderer();
        assert!(!enqueue(image(0, 0)));
        assert!(!enqueue(image(2049, 2048)));
        assert!(matches!(poll(), Poll::Idle));
    }

    #[test]
    fn feature_without_a_registered_renderer_does_not_accept_work() {
        assert!(!enqueue(image(2, 2)));
        assert!(matches!(poll(), Poll::Idle));
        register_renderer();
        assert!(enqueue(image(2, 2)));
        assert!(matches!(poll(), Poll::Deferred));
        clear();
        assert!(matches!(poll(), Poll::Idle));
    }
}

/// Emit bounded renderer cache diagnostics on the current render frame.
pub fn trace(outcome: &'static str, texture_id: Option<u64>, bytes: usize) {
    frame_trace::event(
        "texture_prewarm",
        frame_trace::current_frame(),
        &[
            frame_trace::Field::Str("outcome", outcome),
            frame_trace::Field::U64("texture_id", texture_id.unwrap_or(0)),
            frame_trace::Field::U64("bytes", bytes as u64),
        ],
    );
}

/// Allocate a correlation ID only when tracing is active.
pub fn trace_id() -> Option<u64> {
    frame_trace::enabled().then(frame_trace::next_id)
}
