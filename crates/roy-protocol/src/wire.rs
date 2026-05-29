use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::error::{Result, RoyError};

/// Serialize a value to one newline-terminated JSON frame. Single source of
/// truth for the daemon's line framing — pair with [`decode_line`].
pub fn encode_line<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut buf = serde_json::to_vec(value).map_err(|e| RoyError::Protocol(e.to_string()))?;
    buf.push(b'\n');
    Ok(buf)
}

/// Parse one framed line into a value. Trims surrounding whitespace/newline
/// first, so callers need not remember to `.trim()` — the exact divergence
/// (`roy-gateway` did not trim) this helper exists to remove.
pub fn decode_line<T: DeserializeOwned>(line: &str) -> Result<T> {
    serde_json::from_str(line.trim()).map_err(|e| RoyError::Protocol(e.to_string()))
}

/// Resolve the daemon Unix-socket path: `$ROY_SOCKET` if set, else
/// `$HOME/.roy/daemon.sock`. Single source of truth — replaces six
/// byte-identical copies across the CLI and spokes.
pub fn default_socket_path() -> PathBuf {
    if let Ok(s) = std::env::var("ROY_SOCKET") {
        return PathBuf::from(s);
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".roy/daemon.sock")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ClientCommand;

    #[test]
    fn encode_then_decode_roundtrips() {
        let cmd = ClientCommand::List;
        let frame = encode_line(&cmd).unwrap();
        assert!(frame.ends_with(b"\n"), "frame must be newline-terminated");
        let text = String::from_utf8(frame).unwrap();
        let back: ClientCommand = decode_line(&text).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn decode_line_tolerates_trailing_newline_and_spaces() {
        // The bug this codec exists to kill: some call sites trimmed, one did not.
        let cmd = ClientCommand::List;
        let json = serde_json::to_string(&cmd).unwrap();
        let back: ClientCommand = decode_line(&format!("  {json}\n")).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn decode_line_surfaces_garbage_as_protocol_error() {
        let err = decode_line::<ClientCommand>("{not json").unwrap_err();
        assert!(matches!(err, RoyError::Protocol(_)));
    }
}
