use process_wrap::std::{ChildWrapper, CommandWrap};
use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not start command: {0}")]
    Spawn(io::Error),
    #[error("command I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("command or output collection timed out")]
    TimedOut,
    #[error("command output exceeds configured limit")]
    OutputLimit,
    #[error("command exited with {0}")]
    Exit(ExitStatus),
    #[error("command output reader stopped")]
    ReaderStopped,
}

struct OwnedProcess(Box<dyn ChildWrapper>);

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // Kill this command's group/job, including descendants that inherited its pipes.
        let _ = self.0.start_kill();
        let deadline = Instant::now() + Duration::from_millis(100);
        while matches!(self.0.try_wait(), Ok(None)) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Capture both pipes under one deadline, including after the immediate child exits.
/// Cleanup kills the owned group/job and allows at most 100 ms to reap the leader.
pub fn output(command: Command, timeout: Duration, limit: usize) -> Result<Vec<u8>, Error> {
    let deadline = Instant::now() + timeout;
    let mut command = CommandWrap::from(command);
    #[cfg(unix)]
    command.wrap(process_wrap::std::ProcessGroup::leader());
    #[cfg(windows)]
    command.wrap(process_wrap::std::JobObject);
    command
        .command_mut()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = OwnedProcess(command.spawn().map_err(Error::Spawn)?);
    let stdout = child.0.stdout().take().expect("stdout configured as piped");
    let stderr = child.0.stderr().take().expect("stderr configured as piped");
    let (sender, receiver) = mpsc::channel();
    let capture = |pipe: Box<dyn Read + Send>, stdout| {
        let sender = sender.clone();
        std::thread::spawn(move || {
            let mut pipe = pipe.take(limit as u64 + 1);
            let mut bytes = Vec::new();
            let result = match pipe.read_to_end(&mut bytes) {
                Err(error) => Err(Error::Io(error)),
                Ok(_) if bytes.len() > limit => Err(Error::OutputLimit),
                Ok(_) => Ok(bytes),
            };
            drop(pipe);
            let _ = sender.send((stdout, result));
        });
    };
    capture(Box::new(stdout), true);
    capture(Box::new(stderr), false);
    drop(sender);
    let mut stdout = None;
    let mut stderr_done = false;
    let mut status = None;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Error::TimedOut)?;
        if status.is_none() {
            status = child.0.try_wait()?;
        }
        if let Some(status) = status {
            if !status.success() {
                return Err(Error::Exit(status));
            }
            match stdout {
                Some(bytes) if stderr_done => return Ok(bytes),
                _ => {}
            }
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok((true, result)) => stdout = Some(result?),
            Ok((false, result)) => {
                result?;
                stderr_done = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) if stdout.is_some() && stderr_done => {
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Error::ReaderStopped),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn collects_both_pipes_and_preserves_stdout() {
        assert_eq!(
            output(
                shell("printf fixture; printf diagnostics >&2"),
                Duration::from_secs(2),
                128
            )
            .unwrap(),
            b"fixture"
        );
    }

    #[test]
    fn nonzero_exit_is_not_empty_success() {
        assert!(
            matches!(output(shell("printf failure >&2; exit 7"), Duration::from_secs(2), 128), Err(Error::Exit(status)) if status.code() == Some(7))
        );
    }

    #[test]
    fn deadline_covers_child_and_inherited_stdout_and_stderr() {
        for script in [
            "(sleep 5) 2>/dev/null & printf fixture",
            "(sleep 5) >&2 & printf fixture",
            "exec 1>&- 2>&-; sleep 5",
        ] {
            let start = Instant::now();
            let result = output(shell(script), Duration::from_millis(150), 128);
            assert!(matches!(result, Err(Error::TimedOut)), "{result:?}");
            assert!(
                start.elapsed() < Duration::from_secs(2),
                "descendant pipe defeated deadline"
            );
        }
    }

    #[test]
    fn output_limit_applies_to_each_pipe() {
        for script in ["printf 12345", "printf 12345 >&2"] {
            assert!(matches!(
                output(shell(script), Duration::from_secs(2), 4),
                Err(Error::OutputLimit)
            ));
        }
    }

    #[test]
    fn success_also_cleans_up_owned_background_descendants() {
        let directory = tempfile::tempdir().unwrap();
        let sentinel = directory.path().join("orphan-ran");
        let command = shell(&format!(
            "(sleep 1; touch '{}') >/dev/null 2>&1 & printf fixture",
            sentinel.display()
        ));
        assert_eq!(
            output(command, Duration::from_secs(2), 128).unwrap(),
            b"fixture"
        );
        std::thread::sleep(Duration::from_millis(1200));
        assert!(
            !sentinel.exists(),
            "successful command left its descendant running"
        );
    }

    #[test]
    fn deadline_cleans_up_descendants_before_they_can_act() {
        let directory = tempfile::tempdir().unwrap();
        let sentinel = directory.path().join("orphan-ran");
        let command = shell(&format!("(sleep 1; touch '{}') & wait", sentinel.display()));
        assert!(matches!(
            output(command, Duration::from_millis(100), 128),
            Err(Error::TimedOut)
        ));
        std::thread::sleep(Duration::from_millis(1200));
        assert!(!sentinel.exists(), "timed-out descendant survived cleanup");
    }
}
