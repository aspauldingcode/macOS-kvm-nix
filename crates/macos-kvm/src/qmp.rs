//! QEMU's monitor socket. The install types one command into Recovery, then
//! never touches the keyboard again. The window stays hidden while it does.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};

const HOLD_MS: u64 = 40;
const GAP: Duration = Duration::from_millis(80);

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// QEMU qcodes for one character of the install command.
pub fn keys_for(ch: char) -> Result<Vec<String>> {
    let key = match ch {
        ' ' => vec!["spc".to_owned()],
        '\n' => vec!["ret".to_owned()],
        '/' => vec!["slash".to_owned()],
        '-' => vec!["minus".to_owned()],
        '.' => vec!["dot".to_owned()],
        ';' => vec!["semicolon".to_owned()],
        c if c.is_ascii_uppercase() => {
            vec!["shift".to_owned(), c.to_ascii_lowercase().to_string()]
        }
        c if c.is_ascii_lowercase() || c.is_ascii_digit() => vec![c.to_string()],
        _ => return Err(format!("cannot type {ch:?}").into()),
    };
    Ok(key)
}

pub fn send_key_message(keys: &[String]) -> Value {
    json!({
        "execute": "send-key",
        "arguments": {
            "hold-time": HOLD_MS,
            "keys": keys.iter().map(|key| json!({"type": "qcode", "data": key})).collect::<Vec<_>>()
        }
    })
}

pub struct Qmp {
    stream: UnixStream,
    buf: Vec<u8>,
    pub events: Vec<String>,
}

impl Qmp {
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let mut qmp = Self {
            stream,
            buf: Vec::new(),
            events: Vec::new(),
        };
        let greet = qmp.read_value()?;
        if !greet.to_string().contains("QMP") {
            return Err(format!("not a QMP socket: {greet}").into());
        }
        qmp.execute("qmp_capabilities", None)?;
        Ok(qmp)
    }

    pub fn execute(&mut self, command: &str, arguments: Option<Value>) -> Result<Value> {
        let mut message = json!({"execute": command});
        if let Some(arguments) = arguments {
            message["arguments"] = arguments;
        }
        self.stream.write_all(format!("{message}\n").as_bytes())?;
        self.stream.flush()?;
        loop {
            let reply = self.read_value()?;
            if let Some(event) = reply.get("event").and_then(Value::as_str) {
                self.events.push(event.to_owned());
                continue;
            }
            if let Some(error) = reply.get("error") {
                return Err(format!("QMP {command}: {error}").into());
            }
            if let Some(value) = reply.get("return") {
                return Ok(value.clone());
            }
        }
    }

    pub fn drain(&mut self) -> Result<()> {
        self.stream
            .set_read_timeout(Some(Duration::from_millis(200)))?;
        loop {
            match self.read_value() {
                Ok(reply) => {
                    if let Some(event) = reply.get("event").and_then(Value::as_str) {
                        self.events.push(event.to_owned());
                    }
                }
                Err(_) => break,
            }
        }
        self.stream
            .set_read_timeout(Some(Duration::from_secs(10)))?;
        Ok(())
    }

    pub fn chord(&mut self, keys: &[&str]) -> Result<()> {
        let owned: Vec<String> = keys.iter().map(|key| (*key).to_owned()).collect();
        let message = send_key_message(&owned);
        self.execute("send-key", message.get("arguments").cloned())?;
        Ok(())
    }

    pub fn type_text(&mut self, text: &str) -> Result<()> {
        for ch in text.chars() {
            let keys = keys_for(ch)?;
            let message = send_key_message(&keys);
            self.execute("send-key", message.get("arguments").cloned())?;
            std::thread::sleep(GAP);
        }
        Ok(())
    }

    fn read_value(&mut self) -> Result<Value> {
        loop {
            if let Some(end) = self.buf.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.buf.drain(..=end).collect();
                let text = String::from_utf8_lossy(&line);
                return Ok(serde_json::from_str(text.trim())?);
            }
            let mut chunk = [0u8; 8192];
            let n = self.stream.read(&mut chunk)?;
            if n == 0 {
                return Err("QMP closed".into());
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_install_command_types_on_qcodes() {
        assert_eq!(keys_for('A').unwrap(), ["shift", "a"]);
        assert_eq!(keys_for('/').unwrap(), ["slash"]);
        assert_eq!(keys_for(' ').unwrap(), ["spc"]);
        assert_eq!(keys_for('\n').unwrap(), ["ret"]);
        assert_eq!(keys_for('-').unwrap(), ["minus"]);
        assert_eq!(keys_for('.').unwrap(), ["dot"]);
        assert_eq!(keys_for(';').unwrap(), ["semicolon"]);
        assert_eq!(keys_for('7').unwrap(), ["7"]);
        assert!(keys_for('@').is_err());
        let message = send_key_message(&keys_for('T').unwrap());
        assert_eq!(message["arguments"]["hold-time"], 40);
        assert_eq!(message["arguments"]["keys"][0]["data"], "shift");
        assert_eq!(message["arguments"]["keys"][1]["data"], "t");
    }
}
