use crate::{AppWindow, Frame, Saver};
use slint::{ComponentHandle, Model};

pub(super) fn refresh(app: &AppWindow) {
    texture_prewarm::clear();
    let saver = app.global::<Saver>();
    let frame = app.global::<Frame>();
    let full = saver.get_active();
    let moving = if full {
        saver.get_full_previous() >= 0
    } else {
        saver.get_card_previous() >= 0
    };
    texture_prewarm::set_transition_active(moving);
    if moving || (!full && frame.get_active_tab() != 0) {
        return;
    }
    let photos = frame.get_photos();
    let count = photos.row_count();
    if count < 2 {
        return;
    }
    let current = frame.get_photo_index().max(0) as usize;
    let Some(displayed) = photos.row_data(current) else {
        return;
    };
    let displayed = if full {
        displayed.full
    } else {
        displayed.image
    };
    if displayed.size().width == 0 {
        return;
    }
    let next = (current + 1) % count;
    if let Some(photo) = photos.row_data(next) {
        let image = if full { photo.full } else { photo.image };
        if texture_prewarm::enqueue(image) {
            app.window().request_redraw();
        }
    }
}

#[cfg(all(test, feature = "desktop"))]
mod tests {
    use super::*;
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, PlatformError, WindowAdapter};
    use slint::{Image, ModelRc, Rgb8Pixel, SharedPixelBuffer, VecModel};
    use std::rc::Rc;
    use texture_prewarm::Poll;

    struct TestPlatform;

    impl Platform for TestPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer))
        }
    }

    #[test]
    fn navigation_and_missing_renditions_cancel_deferred_uploads() {
        std::thread::spawn(|| {
            texture_prewarm::register_renderer();
            slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
            let app = AppWindow::new().unwrap();
            let image = Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::new(2, 2));
            let frame = app.global::<Frame>();
            let saver = app.global::<Saver>();
            let model = Rc::new(VecModel::from(vec![
                crate::Photo {
                    image: image.clone(),
                    ..Default::default()
                };
                2
            ]));
            frame.set_photos(ModelRc::from(model.clone()));

            refresh(&app);
            assert!(matches!(texture_prewarm::poll(), Poll::Deferred));
            frame.set_active_tab(5);
            refresh(&app);
            assert!(matches!(texture_prewarm::poll(), Poll::Idle));

            frame.set_active_tab(0);
            refresh(&app);
            assert!(matches!(texture_prewarm::poll(), Poll::Deferred));
            model.set_row_data(1, crate::Photo::default());
            refresh(&app);
            assert!(matches!(texture_prewarm::poll(), Poll::Idle));

            model.set_row_data(1, model.row_data(0).unwrap());
            refresh(&app);
            assert!(matches!(texture_prewarm::poll(), Poll::Deferred));
            saver.set_active(true);
            refresh(&app);
            assert!(matches!(texture_prewarm::poll(), Poll::Idle));
        })
        .join()
        .unwrap();
    }

    #[test]
    fn transition_demand_defers_upload_without_metrics_callbacks() {
        std::thread::spawn(|| {
            texture_prewarm::register_renderer();
            slint::platform::set_platform(Box::new(TestPlatform)).unwrap();
            let app = AppWindow::new().unwrap();
            let image = Image::from_rgb8(SharedPixelBuffer::<Rgb8Pixel>::new(2, 2));
            app.global::<Frame>()
                .set_photos(ModelRc::new(VecModel::from(vec![
                    crate::Photo {
                        full: image.clone(),
                        ..Default::default()
                    };
                    2
                ])));
            let saver = app.global::<Saver>();
            saver.set_active(true);
            saver.set_full_previous(0);
            refresh(&app);
            assert!(matches!(texture_prewarm::poll(), Poll::Idle));
            saver.set_full_previous(-1);
            refresh(&app);
            assert!(matches!(texture_prewarm::poll(), Poll::Deferred));
            assert!(matches!(texture_prewarm::poll(), Poll::Ready(next) if next == image));
        })
        .join()
        .unwrap();
    }
}
