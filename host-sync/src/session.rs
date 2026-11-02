use host_state::LockState;

pub fn current() -> LockState {
    platform::current()
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn output(command: std::process::Command) -> Option<Vec<u8>> {
    host_command::output(command, std::time::Duration::from_secs(2), 1024 * 1024).ok()
}

#[cfg(all(test, unix))]
#[test]
fn a_lock_probe_with_inherited_output_fails_closed_within_its_deadline() {
    let mut command = std::process::Command::new("sh");
    command.args(["-c", "(sleep 5) & printf 'untrusted incomplete probe'"]);
    let start = std::time::Instant::now();
    assert!(output(command).is_none());
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use plist::{Dictionary, Value};

    pub fn current() -> LockState {
        let mut command = std::process::Command::new("/usr/sbin/ioreg");
        command.args(["-n", "Root", "-d1", "-a"]);
        let Some(bytes) = output(command) else {
            return LockState::Unknown;
        };
        let Ok(value) = Value::from_reader(std::io::Cursor::new(bytes)) else {
            return LockState::Unknown;
        };
        // A background process must not expose one user's calendar in another user's console.
        classify(value.as_dictionary(), unsafe { libc::getuid() } as u64)
    }

    fn classify(root: Option<&Dictionary>, uid: u64) -> LockState {
        let Some(root) = root else {
            return LockState::Unknown;
        };
        let active = root
            .get("IOConsoleUsers")
            .and_then(Value::as_array)
            .and_then(|sessions| {
                sessions
                    .iter()
                    .filter_map(Value::as_dictionary)
                    .find(|session| {
                        session
                            .get("kCGSSessionOnConsoleKey")
                            .and_then(Value::as_boolean)
                            == Some(true)
                            && session
                                .get("kCGSSessionUserIDKey")
                                .and_then(Value::as_unsigned_integer)
                                == Some(uid)
                    })
            });
        let Some(session) = active else {
            return LockState::Unknown;
        };
        match (
            root.get("IOConsoleLocked").and_then(Value::as_boolean),
            session
                .get("CGSSessionScreenIsLocked")
                .and_then(Value::as_boolean),
        ) {
            (Some(true), _) | (_, Some(true)) => LockState::Locked,
            (Some(false), _) => LockState::Unlocked,
            _ => LockState::Unknown,
        }
    }

    #[test]
    fn mac_session_requires_own_active_console_and_explicit_unlock() {
        let mut session = Dictionary::new();
        session.insert("kCGSSessionOnConsoleKey".into(), true.into());
        session.insert("kCGSSessionUserIDKey".into(), 501u64.into());
        let mut root = Dictionary::new();
        root.insert(
            "IOConsoleUsers".into(),
            Value::Array(vec![Value::Dictionary(session.clone())]),
        );
        assert_eq!(classify(Some(&root), 501), LockState::Unknown);
        root.insert("IOConsoleLocked".into(), false.into());
        assert_eq!(classify(Some(&root), 501), LockState::Unlocked);
        assert_eq!(classify(Some(&root), 502), LockState::Unknown);
        root.insert("IOConsoleLocked".into(), true.into());
        assert_eq!(classify(Some(&root), 501), LockState::Locked);
        session.insert("CGSSessionScreenIsLocked".into(), true.into());
        root.insert("IOConsoleLocked".into(), false.into());
        root.insert(
            "IOConsoleUsers".into(),
            Value::Array(vec![Value::Dictionary(session)]),
        );
        assert_eq!(classify(Some(&root), 501), LockState::Locked);
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    pub fn current() -> LockState {
        let session = std::env::var("XDG_SESSION_ID").unwrap_or_else(|_| "self".into());
        let mut command = std::process::Command::new("loginctl");
        command.args([
            "show-session",
            &session,
            "--no-pager",
            "--property=Active",
            "--property=LockedHint",
            "--property=Type",
        ]);
        let Some(bytes) = output(command) else {
            return LockState::Unknown;
        };
        classify(&String::from_utf8_lossy(&bytes))
    }
    fn classify(text: &str) -> LockState {
        let property = |name| text.lines().find_map(|line| line.strip_prefix(name));
        if property("Active=") != Some("yes")
            || !matches!(property("Type="), Some("x11" | "wayland"))
        {
            return LockState::Unknown;
        }
        match property("LockedHint=") {
            Some("yes") => LockState::Locked,
            Some("no") => LockState::Unlocked,
            _ => LockState::Unknown,
        }
    }
    #[test]
    fn linux_requires_active_graphical_logind_session() {
        assert_eq!(
            classify("Active=yes\nType=wayland\nLockedHint=no\n"),
            LockState::Unlocked
        );
        assert_eq!(
            classify("Type=x11\nLockedHint=yes\nActive=yes\n"),
            LockState::Locked
        );
        assert_eq!(
            classify("Active=no\nType=wayland\nLockedHint=no\n"),
            LockState::Unknown
        );
        assert_eq!(
            classify("Active=yes\nType=tty\nLockedHint=no\n"),
            LockState::Unknown
        );
        assert_eq!(classify("Active=yes\nType=x11\n"), LockState::Unknown);
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use windows_sys::Win32::System::RemoteDesktop::*;
    pub fn current() -> LockState {
        unsafe {
            let mut buffer = std::ptr::null_mut();
            let mut bytes = 0;
            if WTSQuerySessionInformationW(
                std::ptr::null_mut(),
                WTS_CURRENT_SESSION,
                WTSSessionInfoEx,
                &mut buffer,
                &mut bytes,
            ) == 0
            {
                return LockState::Unknown;
            }
            let result = if !buffer.is_null() && bytes as usize >= std::mem::size_of::<WTSINFOEXW>()
            {
                let info = &*buffer.cast::<WTSINFOEXW>();
                if info.Level != 1 {
                    LockState::Unknown
                } else {
                    let session = info.Data.WTSInfoExLevel1;
                    if session.SessionState != WTSActive {
                        LockState::Unknown
                    } else {
                        match session.SessionFlags as u32 {
                            WTS_SESSIONSTATE_UNLOCK => LockState::Unlocked,
                            WTS_SESSIONSTATE_LOCK => LockState::Locked,
                            _ => LockState::Unknown,
                        }
                    }
                }
            } else {
                LockState::Unknown
            };
            WTSFreeMemory(buffer.cast());
            result
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    use super::*;
    pub fn current() -> LockState {
        LockState::Unknown
    }
}
