use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::session::SessionSnapshot;

pub const MIN_COLS: u16 = 20;
pub const MAX_COLS: u16 = 500;
pub const MIN_ROWS: u16 = 5;
pub const MAX_ROWS: u16 = 300;
pub const MAX_CONTROL_MESSAGE_SIZE: usize = 4 * 1024;
pub const MAX_INPUT_MESSAGE_SIZE: usize = 64 * 1024;
pub const MAX_WEBSOCKET_MESSAGE_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientControl {
    Resize { cols: u16, rows: u16 },
    Ping,
    Restart,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerControl {
    Session {
        session: SessionSnapshot,
    },
    ReplayStart {
        #[serde(rename = "sessionId")]
        session_id: Option<Uuid>,
    },
    ReplayEnd {
        #[serde(rename = "lastSequence")]
        last_sequence: u64,
    },
    Pong,
    Error {
        code: &'static str,
        message: String,
    },
}

pub fn parse_control_message(text: &str) -> Result<ClientControl> {
    if text.len() > MAX_CONTROL_MESSAGE_SIZE {
        bail!("control message is too large");
    }

    let message: ClientControl =
        serde_json::from_str(text).map_err(|_| anyhow::anyhow!("invalid control message"))?;

    if let ClientControl::Resize { cols, rows } = message {
        validate_resize(cols, rows)?;
    }

    Ok(message)
}

pub fn validate_resize(cols: u16, rows: u16) -> Result<()> {
    if !(MIN_COLS..=MAX_COLS).contains(&cols) {
        bail!("cols must be between {MIN_COLS} and {MAX_COLS}");
    }
    if !(MIN_ROWS..=MAX_ROWS).contains(&rows) {
        bail!("rows must be between {MIN_ROWS} and {MAX_ROWS}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resize_control_message() {
        assert_eq!(
            parse_control_message(r#"{"type":"resize","cols":120,"rows":35}"#)
                .expect("valid resize"),
            ClientControl::Resize {
                cols: 120,
                rows: 35
            }
        );
    }

    #[test]
    fn rejects_invalid_resize_values() {
        assert!(validate_resize(19, 35).is_err());
        assert!(validate_resize(120, 301).is_err());
        assert!(validate_resize(120, 35).is_ok());
    }

    #[test]
    fn rejects_unknown_and_damaged_control_messages() {
        assert!(parse_control_message(r#"{"type":"unknown"}"#).is_err());
        assert!(parse_control_message(r#"{"type":"resize","cols":"wide"}"#).is_err());
        assert!(parse_control_message("{").is_err());
    }
}
