# v.00
- Unreleased, no update yet. Build dev builds from source.
- Photos tab: thumbnails appear sooner and scrolling is smoother. Decodes still wanted are no longer restarted on every scrolled row; grid thumbnails stay loaded across tab changes and the screensaver and have their own cache; scrolling one row re-uploads only that row's tiles; tiles round their corners with a mask ring instead of an offscreen-layer clip; the startup scan reads only JPEG headers for the EXIF date.
- Photos tab: grid thumbnails are built once per photo and stored on disk (`FRAME_THUMB_CACHE`), including a background pass while the frame is idle; later visits and restarts load them directly. The production unit stores them in `/var/cache/photoframe-proto/thumbs`.
- Photos tab uses the theme's page background and text colors instead of a fixed dark backdrop.
- Screensaver (portrait): the clock sits at the bottom left, with weather and the next event on the same row at the right edge, matching landscape.
