use anyhow::{bail, ensure, Context, Result};
use calendar_sync::{Snapshot, READY, STORED};
use serde::Deserialize;
use serialport::SerialPort;
use std::io::{Read, Write};
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Connection {
    Console {
        port: String,
        #[serde(default = "default_username")]
        username: String,
        #[serde(default)]
        password_env: Option<String>,
    },
    Raw {
        port: String,
    },
}

fn default_username() -> String {
    "diag".into()
}

impl Connection {
    pub fn validate(&self) -> Result<()> {
        let port = match self {
            Self::Console { port, username, .. } => {
                ensure!(
                    !username.is_empty() && !username.contains(['\r', '\n']),
                    "invalid serial username"
                );
                port
            }
            Self::Raw { port } => port,
        };
        ensure!(!port.is_empty(), "serial port must not be empty");
        Ok(())
    }

    pub fn send(&self, snapshot: &Snapshot) -> Result<()> {
        self.send_message(&snapshot.encode()?, "--calendar-receive", READY, STORED)
    }

    pub fn send_status(&self, status: &host_state::Status) -> Result<()> {
        self.send_message(
            &status.encode()?,
            "--host-state-receive",
            host_state::READY,
            host_state::STORED,
        )
    }

    fn send_message(&self, bytes: &[u8], receiver: &str, ready: &str, stored: &str) -> Result<()> {
        let name = match self {
            Self::Console { port, .. } | Self::Raw { port } => port,
        };
        let mut port = serialport::new(name, 115200)
            .timeout(Duration::from_millis(250))
            .flow_control(serialport::FlowControl::Software)
            .open()
            .with_context(|| {
                format!("could not open serial port {name}; close other console tools first")
            })?;
        match self {
            Self::Console {
                username,
                password_env,
                ..
            } => {
                login(&mut *port, username, password_env.as_deref())?;
                // Keep the existing getty. This shell owns the port only for one bounded exchange.
                writeln!(port, "old=$(stty -g); stty -echo -icanon ixoff; printf '\\n'; timeout 30 /opt/photoframe-proto/current/photoframe_proto {receiver}; stty \"$old\"; printf '\\nFRAME_HOST_DONE_V1\\n'")?;
                port.flush()?;
                let result = exchange_message(&mut port, bytes, ready, stored);
                let restored = wait_for(&mut port, "FRAME_HOST_DONE_V1", Duration::from_secs(35));
                result?;
                restored.context("data stored but shell terminal restoration was not confirmed")?;
            }
            Self::Raw { .. } => exchange_message(&mut port, bytes, ready, stored)?,
        }
        Ok(())
    }
}

fn login(port: &mut dyn SerialPort, username: &str, password_env: Option<&str>) -> Result<()> {
    port.write_all(b"\n")?;
    port.flush()?;
    let greeting = read_for(port, Duration::from_secs(2))?;
    if greeting.contains("login:") {
        writeln!(port, "{username}")?;
        port.flush()?;
        let response = read_for(port, Duration::from_secs(2))?;
        if response.contains("Password:") || response.contains("password:") {
            let env = password_env.context("serial login requires connection.password_env")?;
            let password =
                std::env::var(env).with_context(|| format!("set {env} for serial login"))?;
            ensure!(
                !password.contains(['\r', '\n']),
                "serial password must be one line"
            );
            writeln!(port, "{password}")?;
            port.flush()?;
        }
    } else if greeting.contains("Password:") || greeting.contains("password:") {
        bail!("serial console is waiting for another login; finish or cancel it first");
    }
    port.write_all(b"printf '\\nFRAME_HOST_SHELL_V1\\n'\n")?;
    port.flush()?;
    wait_for(port, "FRAME_HOST_SHELL_V1", Duration::from_secs(10))
        .context("could not establish a logged-in serial shell")
}

fn read_for(port: &mut dyn SerialPort, duration: Duration) -> Result<String> {
    let deadline = Instant::now() + duration;
    let mut output = Vec::new();
    let mut buffer = [0; 1024];
    while Instant::now() < deadline {
        match port.read(&mut buffer) {
            Ok(n) => {
                output.extend_from_slice(&buffer[..n]);
                ensure!(output.len() <= 64 * 1024, "too much serial console output");
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(test)]
pub(crate) fn exchange(stream: &mut (impl Read + Write + ?Sized), bytes: &[u8]) -> Result<()> {
    exchange_message(stream, bytes, READY, STORED)
}

fn exchange_message(
    stream: &mut (impl Read + Write + ?Sized),
    bytes: &[u8],
    ready: &str,
    stored: &str,
) -> Result<()> {
    wait_for(stream, ready, Duration::from_secs(10))?;
    // Pace writes like the deployed console uploader to avoid observed corrupt bulk payloads.
    for chunk in bytes.chunks(2048) {
        stream.write_all(chunk)?;
        stream.flush()?;
        std::thread::sleep(Duration::from_millis(3));
    }
    stream.write_all(b"\n")?;
    stream.flush()?;
    wait_for(stream, stored, Duration::from_secs(15)).context("frame did not confirm persistence")
}

fn wait_for(stream: &mut (impl Read + ?Sized), marker: &str, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut line = Vec::new();
    // Read through the marker only, leaving subsequent handshake bytes for the caller.
    let mut byte = [0];
    while Instant::now() < deadline {
        match stream.read(&mut byte) {
            Ok(0) => bail!("serial stream closed before {marker}"),
            Ok(_) if byte[0] == b'\n' => {
                let text = String::from_utf8_lossy(&line);
                if text.trim_end_matches('\r') == marker {
                    return Ok(());
                }
                ensure!(
                    !matches!(
                        text.trim_end_matches('\r'),
                        "FRAME_CALENDAR_ERROR_V1" | host_state::ERROR
                    ),
                    "frame rejected update"
                );
                line.clear();
            }
            Ok(_) => {
                line.push(byte[0]);
                ensure!(line.len() <= 64 * 1024, "serial response line too long");
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    bail!("timed out waiting for {marker}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct Wire {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Wire {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(b)
        }
    }
    impl Write for Wire {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.output.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn requires_ready_and_storage_ack_not_echoed_commands() {
        let mut wire = Wire {
            input: Cursor::new(format!("echo {READY}\r\n{READY}\r\n{STORED}\r\n").into_bytes()),
            output: vec![],
        };
        exchange(&mut wire, b"{}").unwrap();
        assert_eq!(wire.output, b"{}\n");
        wire.input = Cursor::new(format!("{READY}\nFRAME_CALENDAR_ERROR_V1\n").into_bytes());
        assert!(exchange(&mut wire, b"{}").is_err());
        wire.input = Cursor::new(format!("echo {READY}\n").into_bytes());
        wire.output.clear();
        assert!(exchange(&mut wire, b"{}").is_err());
        assert!(wire.output.is_empty());
    }
}
