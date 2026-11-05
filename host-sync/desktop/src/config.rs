use anyhow::{ensure, Context, Result};
use frame_host_sync::Config;
use std::io::Write;
use std::path::{Path, PathBuf};

pub fn initialize(home: &Path) -> Result<(PathBuf, bool)> {
    let directory = home.join(".config/home-dash");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&directory)?;
    let path = directory.join("host-sync.json");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(include_bytes!("../default-config.json"))?;
            Ok((path, true))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok((path, false)),
        Err(error) => Err(error.into()),
    }
}

pub fn save(path: &Path, contents: &[u8]) -> Result<()> {
    let config = Config::parse(contents)?;
    ensure!(
        config.connection.is_some(),
        "Set connection.port before saving."
    );
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .context("Could not replace configuration.")?;
    Ok(())
}

pub fn import(target: &Path, source: &Path) -> Result<()> {
    let contents = std::fs::read(source)?;
    let config = Config::parse(&contents)?;
    let mut json: serde_json::Value = serde_json::from_slice(&contents)?;
    let base = source
        .parent()
        .context("Imported configuration has no parent directory.")?;
    for (index, source) in config.calendar.sources.iter().enumerate() {
        if let calendar_sync::sources::SourceConfig::Json { path, .. } = source {
            if path.is_relative() {
                json["calendar"]["sources"][index]["path"] = serde_json::to_value(base.join(path))?;
            }
        }
    }
    save(target, &serde_json::to_vec_pretty(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &[u8] =
        br#"{"connection":{"kind":"raw","port":"test"},"calendar":{"sources":[]}}"#;

    #[test]
    fn first_launch_creates_default_and_later_launch_preserves_edits() {
        let home = tempfile::tempdir().unwrap();
        let (path, created) = initialize(home.path()).unwrap();
        assert!(created);
        assert_eq!(path, home.path().join(".config/home-dash/host-sync.json"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            include_bytes!("../default-config.json")
        );
        save(&path, VALID).unwrap();
        assert!(!initialize(home.path()).unwrap().1);
        assert_eq!(std::fs::read(&path).unwrap(), VALID);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_save_does_not_replace_configuration() {
        let home = tempfile::tempdir().unwrap();
        let (path, _) = initialize(home.path()).unwrap();
        save(&path, VALID).unwrap();
        for contents in [
            b"not json".as_slice(),
            br#"{"calendar":{"sources":[]}}"#,
            include_bytes!("../default-config.json"),
        ] {
            assert!(save(&path, contents).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), VALID);
        }
    }

    #[cfg(unix)]
    #[test]
    fn failed_writes_preserve_previous_configuration() {
        const CHILD: &str = "FRAME_CONFIG_WRITE_FAILURE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "config::tests::failed_writes_preserve_previous_configuration",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let (path, _) = initialize(home.path()).unwrap();
        save(&path, VALID).unwrap();
        let source = home.path().join("import.json");
        std::fs::write(&source, VALID).unwrap();
        let before = std::fs::read(&path).unwrap();
        let mut original = unsafe { std::mem::zeroed::<libc::rlimit>() };
        unsafe {
            assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut original), 0);
            libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
            let limited = libc::rlimit {
                rlim_cur: 16,
                rlim_max: original.rlim_max,
            };
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limited), 0);
        }
        let saved = save(&path, VALID);
        let imported = import(&path, &source);
        unsafe {
            assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &original), 0);
        }
        assert!(saved.is_err());
        assert!(imported.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[test]
    fn importing_preserves_relative_source_file_resolution() {
        let home = tempfile::tempdir().unwrap();
        let (target, _) = initialize(home.path()).unwrap();
        let source = home.path().join("original.json");
        std::fs::write(&source, br#"{"connection":{"kind":"raw","port":"test"},"calendar":{"sources":[{"kind":"json","id":"personal","path":"events.json"}]}}"#).unwrap();
        import(&target, &source).unwrap();
        let config = Config::load(&target).unwrap();
        let calendar_sync::sources::SourceConfig::Json { path, .. } = &config.calendar.sources[0]
        else {
            panic!("wrong source")
        };
        assert_eq!(*path, home.path().join("events.json"));
        assert!(source.exists());
    }
}
