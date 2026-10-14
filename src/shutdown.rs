use std::io;

pub fn block_termination() -> io::Result<libc::sigset_t> {
    unsafe {
        let mut signals = std::mem::zeroed();
        libc::sigemptyset(&mut signals);
        libc::sigaddset(&mut signals, libc::SIGTERM);
        libc::sigaddset(&mut signals, libc::SIGINT);
        let error = libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut());
        if error != 0 { return Err(io::Error::from_raw_os_error(error)); }
        Ok(signals)
    }
}

pub fn listen(signals: libc::sigset_t) -> io::Result<()> {
    std::thread::Builder::new().name("frame-shutdown".into()).spawn(move || {
        let mut signal = 0;
        let error = unsafe { libc::sigwait(&signals, &mut signal) };
        if error != 0 {
            eprintln!("[shutdown] sigwait failed: {}", io::Error::from_raw_os_error(error));
            return;
        }
        if let Err(error) = slint::invoke_from_event_loop(|| {
            if let Err(error) = slint::quit_event_loop() {
                eprintln!("[shutdown] cannot quit event loop: {error}");
            }
        }) {
            eprintln!("[shutdown] cannot schedule shutdown: {error}");
        }
    })?;
    Ok(())
}
