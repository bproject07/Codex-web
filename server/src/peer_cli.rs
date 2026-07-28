use std::{
    env,
    ffi::OsString,
    fs::File,
    io::{self, IsTerminal, Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::peer::{
    CWT_PEER_CAPABILITY_ENV, CWT_PEER_ENDPOINT_ENV, MAX_PEER_ARTIFACT_BYTES,
    peer_artifact_has_unsafe_control,
};

pub const PEER_CLI_COMMAND: &str = "__cwt-peer";
pub const INTERNAL_PEER_PATH: &str = "/internal/v1/peer";
pub const PEER_CAPABILITY_SCHEME: &str = "CWT-Capability";

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_HEADER_BYTES: usize = 32 * 1024;
// A valid 64 KiB UTF-8 artifact can expand to six JSON bytes per input byte
// when every byte requires a `\u00xx` escape. Keep the decoded domain limit
// strict while allowing that worst-case wire representation plus metadata.
const MAX_HTTP_RESPONSE_BYTES: usize = MAX_PEER_ARTIFACT_BYTES * 6 + 16 * 1024;
const MAX_HTTP_REQUEST_BYTES: usize = MAX_PEER_ARTIFACT_BYTES * 6 + 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum InternalPeerRequest {
    Submit { turn_id: Uuid, content: String },
    Receive { turn_id: Uuid },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternalPeerResponse {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

enum PeerCliCommand {
    Submit { turn_id: Uuid, input: SubmitInput },
    Receive { turn_id: Uuid },
}

enum SubmitInput {
    File(PathBuf),
    Stdin,
}

/// Executes the private helper command when argv starts with `__cwt-peer`.
///
/// Call this before parsing the public server CLI. `Ok(false)` means the
/// process is a normal server invocation. Capability values are read only from
/// the inherited environment and are never accepted as argv.
pub fn try_run_from_environment() -> Result<bool> {
    let mut arguments = env::args_os();
    let _program = arguments.next();
    if arguments
        .next()
        .as_deref()
        .and_then(|argument| argument.to_str())
        != Some(PEER_CLI_COMMAND)
    {
        return Ok(false);
    }

    let command = parse_arguments(arguments.collect())?;
    execute(command)?;
    Ok(true)
}

fn execute(command: PeerCliCommand) -> Result<()> {
    let (request, received_turn) = match command {
        PeerCliCommand::Submit { turn_id, input } => {
            let content = match input {
                SubmitInput::File(path) => read_bounded_file(&path)?,
                SubmitInput::Stdin => read_bounded_stdin()?,
            };
            (InternalPeerRequest::Submit { turn_id, content }, None)
        }
        PeerCliCommand::Receive { turn_id } => {
            (InternalPeerRequest::Receive { turn_id }, Some(turn_id))
        }
    };

    let endpoint = read_endpoint()?;
    let capability = read_capability()?;
    let response = send_request(endpoint, &capability, &request)?;

    if let Some(turn_id) = received_turn {
        let content = response
            .content
            .context("the peer broker returned no artifact content")?;
        validate_content(&content)?;
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(content.as_bytes())
            .context("failed to write the peer artifact")?;
        if !content.ends_with('\n') {
            stdout
                .write_all(b"\n")
                .context("failed to finish the peer artifact output")?;
        }
        stdout.flush().context("failed to flush peer output")?;
        let _ = turn_id;
    }
    Ok(())
}

fn parse_arguments(arguments: Vec<OsString>) -> Result<PeerCliCommand> {
    let mut arguments = arguments.into_iter();
    let operation = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .context("expected `submit` or `receive` after __cwt-peer")?;

    let mut turn_id = None;
    let mut input = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--turn") => {
                if turn_id.is_some() {
                    bail!("--turn may be supplied only once");
                }
                let value = arguments
                    .next()
                    .and_then(|value| value.into_string().ok())
                    .context("--turn requires a UTF-8 UUID value")?;
                turn_id = Some(Uuid::parse_str(&value).context("--turn is not a valid UUID")?);
            }
            Some("--file") => {
                if input.is_some() {
                    bail!("choose exactly one of --file or --stdin");
                }
                let path = arguments.next().context("--file requires a path")?;
                if path.is_empty() {
                    bail!("--file path must not be empty");
                }
                input = Some(SubmitInput::File(PathBuf::from(path)));
            }
            Some("--stdin") => {
                if input.is_some() {
                    bail!("choose exactly one of --file or --stdin");
                }
                input = Some(SubmitInput::Stdin);
            }
            _ => bail!("unrecognized __cwt-peer argument"),
        }
    }

    let turn_id = turn_id.context("--turn is required")?;
    match operation.as_str() {
        "submit" => Ok(PeerCliCommand::Submit {
            turn_id,
            input: input.context("submit requires exactly one of --file or --stdin")?,
        }),
        "receive" => {
            if input.is_some() {
                bail!("receive does not accept --file or --stdin");
            }
            Ok(PeerCliCommand::Receive { turn_id })
        }
        _ => bail!("expected `submit` or `receive` after __cwt-peer"),
    }
}

fn read_endpoint() -> Result<SocketAddr> {
    let raw = env::var(CWT_PEER_ENDPOINT_ENV)
        .context("peer helper endpoint is not available in this terminal")?;
    let endpoint: SocketAddr = raw
        .parse()
        .context("peer helper endpoint is not a socket address")?;
    validate_endpoint(endpoint)?;
    Ok(endpoint)
}

fn validate_endpoint(endpoint: SocketAddr) -> Result<()> {
    if !endpoint.ip().is_loopback() || endpoint.port() == 0 {
        bail!("peer helper endpoint must use a loopback address and non-zero port");
    }
    Ok(())
}

fn read_capability() -> Result<String> {
    let capability = env::var(CWT_PEER_CAPABILITY_ENV)
        .context("peer capability is not available in this terminal")?;
    if !(16..=512).contains(&capability.len())
        || !capability
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        bail!("peer capability has an invalid format");
    }
    Ok(capability)
}

fn read_bounded_file(path: &Path) -> Result<String> {
    let file = File::open(path)
        .with_context(|| format!("failed to open the peer artifact file: {}", path.display()))?;
    read_bounded(file)
        .with_context(|| format!("failed to read the peer artifact file: {}", path.display()))
}

fn read_bounded_stdin() -> Result<String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        bail!("--stdin requires redirected or piped UTF-8 input");
    }
    read_bounded(stdin.lock()).context("failed to read the peer artifact from stdin")
}

fn read_bounded(reader: impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_PEER_ARTIFACT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("failed to read peer artifact bytes")?;
    if bytes.len() > MAX_PEER_ARTIFACT_BYTES {
        bail!("peer artifact exceeds the 64 KiB limit");
    }
    let content = String::from_utf8(bytes).context("peer artifact must be valid UTF-8")?;
    validate_content(&content)?;
    Ok(content)
}

fn validate_content(content: &str) -> Result<()> {
    if content.len() > MAX_PEER_ARTIFACT_BYTES {
        bail!("peer artifact exceeds the 64 KiB limit");
    }
    if content.trim().is_empty() || content.contains('\0') {
        bail!("peer artifact must contain non-empty UTF-8 text without NUL bytes");
    }
    if peer_artifact_has_unsafe_control(content) {
        bail!("peer artifact contains an unsafe control character");
    }
    Ok(())
}

fn send_request(
    endpoint: SocketAddr,
    capability: &str,
    request: &InternalPeerRequest,
) -> Result<InternalPeerResponse> {
    validate_endpoint(endpoint)?;
    let body = serde_json::to_vec(request).context("failed to encode peer helper request")?;
    if body.len() > MAX_HTTP_REQUEST_BYTES {
        bail!("encoded peer helper request exceeds its transport limit");
    }

    let mut stream = TcpStream::connect_timeout(&endpoint, IO_TIMEOUT)
        .context("failed to connect to the local peer broker")?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .context("failed to configure the peer broker read timeout")?;
    stream
        .set_write_timeout(Some(IO_TIMEOUT))
        .context("failed to configure the peer broker write timeout")?;

    write!(
        stream,
        "POST {INTERNAL_PEER_PATH} HTTP/1.1\r\n\
         Host: {endpoint}\r\n\
         Authorization: {PEER_CAPABILITY_SCHEME} {capability}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )
    .context("failed to write peer broker request headers")?;
    stream
        .write_all(&body)
        .context("failed to write peer broker request body")?;
    stream
        .flush()
        .context("failed to flush peer broker request")?;

    let mut response = Vec::new();
    stream
        .take((MAX_HTTP_HEADER_BYTES + MAX_HTTP_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut response)
        .context("failed to read peer broker response")?;
    parse_http_response(&response)
}

fn parse_http_response(response: &[u8]) -> Result<InternalPeerResponse> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("peer broker returned an invalid HTTP response")?;
    if header_end > MAX_HTTP_HEADER_BYTES {
        bail!("peer broker response headers exceed their limit");
    }
    let headers = std::str::from_utf8(&response[..header_end])
        .context("peer broker returned non-UTF-8 HTTP headers")?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .context("peer broker returned an invalid HTTP status")?;
    let body = decode_http_body(headers, &response[header_end..])?;
    if body.len() > MAX_HTTP_RESPONSE_BYTES {
        bail!("peer broker response exceeds its transport limit");
    }

    let decoded = if body.is_empty() {
        InternalPeerResponse {
            content: None,
            error: None,
        }
    } else {
        serde_json::from_slice::<InternalPeerResponse>(&body)
            .context("peer broker returned invalid JSON")?
    };
    if !(200..300).contains(&status) {
        let message = decoded
            .error
            .as_deref()
            .unwrap_or("the local peer broker rejected the request");
        bail!("peer broker request failed with HTTP {status}: {message}");
    }
    Ok(decoded)
}

fn decode_http_body(headers: &str, body: &[u8]) -> Result<Vec<u8>> {
    let chunked = headers.lines().skip(1).any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding")
                && value
                    .split(',')
                    .any(|encoding| encoding.trim().eq_ignore_ascii_case("chunked"))
        })
    });
    if chunked {
        return decode_chunked(body);
    }

    let content_length = headers.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    if let Some(length) = content_length {
        if length > MAX_HTTP_RESPONSE_BYTES || body.len() < length {
            bail!("peer broker returned an invalid Content-Length");
        }
        return Ok(body[..length].to_vec());
    }
    Ok(body.to_vec())
}

fn decode_chunked(mut input: &[u8]) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    loop {
        let line_end = input
            .windows(2)
            .position(|window| window == b"\r\n")
            .context("peer broker returned invalid chunk framing")?;
        let size_text = std::str::from_utf8(&input[..line_end])
            .context("peer broker returned an invalid chunk size")?
            .split(';')
            .next()
            .unwrap_or_default();
        let size = usize::from_str_radix(size_text.trim(), 16)
            .context("peer broker returned an invalid chunk size")?;
        input = &input[line_end + 2..];
        if size == 0 {
            break;
        }
        if size > MAX_HTTP_RESPONSE_BYTES.saturating_sub(output.len())
            || input.len() < size + 2
            || &input[size..size + 2] != b"\r\n"
        {
            bail!("peer broker returned an invalid or oversized chunk");
        }
        output.extend_from_slice(&input[..size]);
        input = &input[size + 2..];
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_submit_file_and_receive_commands() {
        let turn_id = Uuid::new_v4();
        let submit = parse_arguments(vec![
            OsString::from("submit"),
            OsString::from("--turn"),
            OsString::from(turn_id.to_string()),
            OsString::from("--file"),
            OsString::from("handoff.md"),
        ])
        .expect("submit args");
        assert!(matches!(
            submit,
            PeerCliCommand::Submit {
                turn_id: parsed,
                input: SubmitInput::File(_)
            } if parsed == turn_id
        ));

        let receive = parse_arguments(vec![
            OsString::from("receive"),
            OsString::from("--turn"),
            OsString::from(turn_id.to_string()),
        ])
        .expect("receive args");
        assert!(matches!(
            receive,
            PeerCliCommand::Receive { turn_id: parsed } if parsed == turn_id
        ));
    }

    #[test]
    fn rejects_ambiguous_input_and_oversized_content() {
        let turn_id = Uuid::new_v4();
        assert!(
            parse_arguments(vec![
                OsString::from("submit"),
                OsString::from("--turn"),
                OsString::from(turn_id.to_string()),
                OsString::from("--stdin"),
                OsString::from("--file"),
                OsString::from("handoff.md"),
            ])
            .is_err()
        );
        assert!(validate_content(&"x".repeat(MAX_PEER_ARTIFACT_BYTES + 1)).is_err());
    }

    #[test]
    fn helper_accepts_multiline_unicode_but_rejects_terminal_controls() {
        assert!(validate_content("Резюме\r\n- проверка\t✓").is_ok());
        for content in [
            "bell\u{0007}",
            "escape\u{001b}]52;c;payload\u{0007}",
            "delete\u{007f}",
            "c1-csi\u{009b}2J",
        ] {
            assert!(validate_content(content).is_err());
        }
    }

    #[test]
    fn accepts_only_loopback_endpoints() {
        assert!(validate_endpoint("127.0.0.1:1234".parse().expect("endpoint")).is_ok());
        assert!(validate_endpoint("[::1]:1234".parse().expect("endpoint")).is_ok());
        assert!(validate_endpoint("192.0.2.10:1234".parse().expect("endpoint")).is_err());
        assert!(validate_endpoint("127.0.0.1:0".parse().expect("endpoint")).is_err());
    }

    #[test]
    fn parses_content_length_and_chunked_responses() {
        let direct = b"HTTP/1.1 200 OK\r\nContent-Length: 16\r\n\r\n{\"content\":\"ok\"}";
        assert_eq!(
            parse_http_response(direct)
                .expect("direct response")
                .content
                .as_deref(),
            Some("ok")
        );

        let chunked =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\n{\"content\":\"ok\"}\r\n0\r\n\r\n";
        assert_eq!(
            parse_http_response(chunked)
                .expect("chunked response")
                .content
                .as_deref(),
            Some("ok")
        );
    }
}
