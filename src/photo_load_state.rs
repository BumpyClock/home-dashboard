/// Physical photo width, photo height, and caption-scrim height.
pub type PhotoSize = (u32, u32, u32);

#[derive(Default)]
pub struct PhotoLoadState {
    pub generation: u64,
    completed: Option<PhotoSize>,
    active: Option<PhotoSize>,
    published: bool,
    baked: bool,
}

impl PhotoLoadState {
    pub const fn new() -> Self {
        Self { generation: 0, completed: None, active: None, published: false, baked: false }
    }

    pub fn request_mode(&mut self, size: PhotoSize, baked: bool) -> Option<u64> {
        if self.baked != baked {
            self.completed = None;
            self.active = None;
            self.published = false;
            self.baked = baked;
        }
        self.request(size)
    }

    pub fn request(&mut self, size: PhotoSize) -> Option<u64> {
        if self.completed == Some(size) {
            if self.active.is_some_and(|active| active != size) {
                self.generation += 1;
                self.active = None;
                self.published = false;
            }
            return None;
        }
        if self.active == Some(size) {
            return None;
        }
        self.generation += 1;
        self.active = Some(size);
        self.published = false;
        Some(self.generation)
    }

    pub fn ready(&mut self, generation: u64, size: PhotoSize) {
        if generation == self.generation && self.active == Some(size) {
            self.published = true;
            self.completed = None;
        }
    }

    pub fn finish(&mut self, generation: u64) {
        if generation == self.generation {
            if self.published {
                self.completed = self.active;
            }
            self.active = None;
            self.published = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PhotoLoadState;

    #[test]
    fn scrim_mode_change_cancels_previous_batch_at_same_size() {
        let mut state = PhotoLoadState::new();
        let size = (388, 280, 180);
        let plain = state.request_mode(size, false).unwrap();
        let baked = state.request_mode(size, true).unwrap();
        assert!(baked > plain);
        state.ready(plain, size);
        state.finish(plain);
        state.ready(baked, size);
        state.finish(baked);
        assert_eq!(state.request_mode(size, true), None);
        assert!(state.request_mode(size, false).is_some());
    }

    #[test]
    fn completed_size_skips_reload_but_failed_resize_can_retry() {
        let mut state = PhotoLoadState::default();
        let first = state.request((388, 280, 180)).unwrap();
        state.ready(first, (388, 280, 180));
        state.finish(first);
        assert_eq!(state.request((388, 280, 180)), None);
        let resized = state.request((600, 400, 180)).unwrap();
        state.finish(resized);
        assert!(state.request((600, 400, 180)).is_some());
    }

    #[test]
    fn duplicate_request_does_not_cancel_remaining_batch() {
        let mut state = PhotoLoadState::default();
        let first = state.request((388, 280, 180)).unwrap();
        assert_eq!(state.request((388, 280, 180)), None);
        state.ready(first, (388, 280, 180));
        assert_eq!(state.request((388, 280, 180)), None);
        assert_eq!(state.generation, first);
        state.finish(first);
        assert_eq!(state.request((388, 280, 180)), None);
    }

    #[test]
    fn returning_to_cached_size_cancels_outdated_resize() {
        let mut state = PhotoLoadState::default();
        let first = state.request((388, 280, 180)).unwrap();
        state.ready(first, (388, 280, 180));
        state.finish(first);
        let resized = state.request((600, 400, 180)).unwrap();
        assert_eq!(state.request((388, 280, 180)), None);
        assert!(state.generation > resized);
        state.ready(resized, (600, 400, 180));
        state.finish(resized);
        assert_eq!(state.request((388, 280, 180)), None);
        assert!(state.request((600, 400, 180)).is_some());
    }

    #[test]
    fn interrupted_partial_batch_reloads_when_returning_to_its_size() {
        let mut state = PhotoLoadState::new();
        let first = state.request((388, 280, 180)).unwrap();
        state.ready(first, (388, 280, 180));
        let resized = state.request((600, 400, 180)).unwrap();
        let returned = state.request((388, 280, 180)).unwrap();
        assert!(returned > resized);
        state.finish(first);
        state.finish(resized);
        assert_eq!(state.request((388, 280, 180)), None);
        state.finish(returned);
        assert!(state.request((388, 280, 180)).is_some());
    }

    #[test]
    fn publishing_resize_invalidates_previous_completed_size() {
        let mut state = PhotoLoadState::new();
        let first = state.request((388, 280, 180)).unwrap();
        state.ready(first, (388, 280, 180));
        state.finish(first);
        let resized = state.request((600, 400, 180)).unwrap();
        state.ready(resized, (600, 400, 180));
        let returned = state.request((388, 280, 180)).unwrap();
        assert!(returned > resized);
        state.finish(resized);
        state.ready(returned, (388, 280, 180));
        state.finish(returned);
        assert_eq!(state.request((388, 280, 180)), None);
        assert!(state.request((600, 400, 180)).is_some());
    }

    #[test]
    fn changed_scrim_height_reloads_same_photo_dimensions() {
        let mut state = PhotoLoadState::new();
        let first = state.request((388, 280, 180)).unwrap();
        state.ready(first, (388, 280, 180));
        state.finish(first);
        let resized_scrim = state.request((388, 280, 90)).unwrap();
        assert!(resized_scrim > first);
        state.ready(resized_scrim, (388, 280, 90));
        state.finish(resized_scrim);
        assert_eq!(state.request((388, 280, 90)), None);
        assert!(state.request((388, 280, 180)).is_some());
    }
}
