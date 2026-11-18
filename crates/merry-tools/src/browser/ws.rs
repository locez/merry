//! Minimal, safe RFC 6455 WebSocket client codec for loopback CDP connections.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{
        AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf,
        WriteHalf,
    },
    net::TcpStream,
};
use url::Url;

static NONCE_COUNTER: AtomicU64 = AtomicU64::new(1);

const OPCODE_CONTINUATION: u8 = 0x0;
const OPCODE_TEXT: u8 = 0x1;
const OPCODE_BINARY: u8 = 0x2;
const OPCODE_CLOSE: u8 = 0x8;
const OPCODE_PING: u8 = 0x9;
const OPCODE_PONG: u8 = 0xA;
const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;

/// A split WebSocket message returned by [`WsReader::next_message`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WsMessage {
    Text(String),
    Ping(Vec<u8>),
    Close,
}

/// Reader half of a WebSocket connection.
pub(crate) struct WsReader<R> {
    reader: BufReader<R>,
}

/// Writer half of a WebSocket connection.
pub(crate) struct WsWriter<W> {
    writer: W,
}

/// Connects to a plain `ws://127.0.0.1:<port>/...` or `ws://localhost:<port>/...` CDP endpoint.
pub(crate) async fn connect_tcp_ws(
    ws_url: &str,
) -> Result<
    (
        WsReader<ReadHalf<TcpStream>>,
        WsWriter<WriteHalf<TcpStream>>,
    ),
    String,
> {
    let url =
        Url::parse(ws_url).map_err(|e| format!("invalid CDP WebSocket URL {ws_url:?}: {e}"))?;
    if url.scheme() != "ws" {
        return Err(format!(
            "unsupported CDP WebSocket scheme {:?}: expected ws://",
            url.scheme()
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| "CDP WebSocket URL missing host".to_owned())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "CDP WebSocket URL missing port".to_owned())?;
    let path = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    };
    let host_header = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };

    let stream = TcpStream::connect((host, port))
        .await
        .map_err(|e| format!("failed to connect to CDP WebSocket {ws_url}: {e}"))?;
    handshake_ws(stream, &host_header, &path).await
}

/// Performs the HTTP/1.1 WebSocket Upgrade handshake over any async stream.
pub(crate) async fn handshake_ws<S>(
    mut stream: S,
    host_header: &str,
    path: &str,
) -> Result<(WsReader<ReadHalf<S>>, WsWriter<WriteHalf<S>>), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let key = next_ws_key();
    let req_path = if path.is_empty() { "/" } else { path };
    let request = format!(
        "GET {req_path} HTTP/1.1\r\n\
         Host: {host_header}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("failed to write WebSocket upgrade request: {e}"))?;
    stream
        .flush()
        .await
        .map_err(|e| format!("failed to flush WebSocket upgrade request: {e}"))?;

    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut status_line = String::new();
    let n = reader
        .read_line(&mut status_line)
        .await
        .map_err(|e| format!("failed to read WebSocket upgrade response: {e}"))?;
    if n == 0 {
        return Err("CDP WebSocket closed before sending HTTP upgrade status".to_owned());
    }
    if !status_line.contains(" 101 ") && !status_line.contains(" 101\r") {
        return Err(format!(
            "unexpected WebSocket upgrade status: {}",
            status_line.trim()
        ));
    }
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("failed reading WebSocket headers: {e}"))?;
        if read == 0 {
            return Err("unexpected EOF while reading WebSocket upgrade headers".to_owned());
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
    }

    Ok((WsReader { reader }, WsWriter { writer: write_half }))
}

impl<R: AsyncRead + Unpin> WsReader<R> {
    /// Reads the next complete WebSocket message (handling fragmented text frames and control frames).
    pub(crate) async fn next_message(&mut self) -> Result<WsMessage, String> {
        let mut fragmented: Option<(u8, Vec<u8>)> = None;

        loop {
            let mut header = [0u8; 2];
            match self.reader.read_exact(&mut header).await {
                Ok(_) => {}
                Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(WsMessage::Close);
                }
                Err(err) => return Err(format!("WebSocket read error: {err}")),
            }

            let fin = (header[0] & 0x80) != 0;
            let opcode = header[0] & 0x0F;
            let masked = (header[1] & 0x80) != 0;
            let len_code = header[1] & 0x7F;

            let payload_len = match len_code {
                0..=125 => usize::from(len_code),
                126 => {
                    let mut ext = [0u8; 2];
                    self.reader
                        .read_exact(&mut ext)
                        .await
                        .map_err(|e| format!("failed reading 16-bit frame length: {e}"))?;
                    usize::from(u16::from_be_bytes(ext))
                }
                127 => {
                    let mut ext = [0u8; 8];
                    self.reader
                        .read_exact(&mut ext)
                        .await
                        .map_err(|e| format!("failed reading 64-bit frame length: {e}"))?;
                    let len64 = u64::from_be_bytes(ext);
                    usize::try_from(len64)
                        .map_err(|_| "WebSocket frame exceeds addressable size".to_owned())?
                }
                _ => unreachable!("7-bit length code"),
            };

            if payload_len > MAX_FRAME_BYTES {
                return Err(format!("WebSocket frame too large ({payload_len} bytes)"));
            }

            let mask_key = if masked {
                let mut key = [0u8; 4];
                self.reader
                    .read_exact(&mut key)
                    .await
                    .map_err(|e| format!("failed reading WebSocket mask: {e}"))?;
                Some(key)
            } else {
                None
            };

            let mut payload = vec![0u8; payload_len];
            self.reader
                .read_exact(&mut payload)
                .await
                .map_err(|e| format!("failed reading WebSocket payload: {e}"))?;

            if let Some(key) = mask_key {
                apply_mask(&mut payload, key);
            }

            match opcode {
                OPCODE_PING => return Ok(WsMessage::Ping(payload)),
                OPCODE_PONG => continue,
                OPCODE_CLOSE => return Ok(WsMessage::Close),
                OPCODE_TEXT | OPCODE_BINARY => {
                    if fin {
                        if opcode == OPCODE_TEXT {
                            let text = String::from_utf8(payload).map_err(|e| {
                                format!("invalid UTF-8 in WebSocket text frame: {e}")
                            })?;
                            return Ok(WsMessage::Text(text));
                        }
                        continue;
                    }
                    fragmented = Some((opcode, payload));
                }
                OPCODE_CONTINUATION => {
                    let Some((orig_opcode, mut acc)) = fragmented.take() else {
                        return Err("unexpected WebSocket continuation frame".to_owned());
                    };
                    if acc.len().saturating_add(payload.len()) > MAX_FRAME_BYTES {
                        return Err("fragmented WebSocket message exceeds maximum size".to_owned());
                    }
                    acc.extend_from_slice(&payload);
                    if fin {
                        if orig_opcode == OPCODE_TEXT {
                            let text = String::from_utf8(acc).map_err(|e| {
                                format!("invalid UTF-8 in fragmented text frame: {e}")
                            })?;
                            return Ok(WsMessage::Text(text));
                        }
                    } else {
                        fragmented = Some((orig_opcode, acc));
                    }
                }
                _ => {
                    return Err(format!("unsupported WebSocket opcode {opcode:#x}"));
                }
            }
        }
    }
}

impl<W: AsyncWrite + Unpin> WsWriter<W> {
    /// Sends a masked client text frame.
    pub(crate) async fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.write_masked_frame(OPCODE_TEXT, text.as_bytes()).await
    }

    /// Sends a masked client pong frame.
    pub(crate) async fn send_pong(&mut self, payload: &[u8]) -> Result<(), String> {
        self.write_masked_frame(OPCODE_PONG, payload).await
    }

    /// Sends a masked client close frame.
    pub(crate) async fn send_close(&mut self) -> Result<(), String> {
        self.write_masked_frame(OPCODE_CLOSE, &[]).await
    }

    async fn write_masked_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let len = payload.len();
        let mut frame = Vec::with_capacity(14 + len);
        frame.push(0x80 | opcode);
        if len < 126 {
            frame.push(0x80 | (len as u8));
        } else if let Ok(len16) = u16::try_from(len) {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&len16.to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }

        let mask = next_mask_key();
        frame.extend_from_slice(&mask);
        let payload_start = frame.len();
        frame.extend_from_slice(payload);
        apply_mask(&mut frame[payload_start..], mask);

        self.writer
            .write_all(&frame)
            .await
            .map_err(|e| format!("failed to write WebSocket frame: {e}"))?;
        self.writer
            .flush()
            .await
            .map_err(|e| format!("failed to flush WebSocket frame: {e}"))
    }
}

fn apply_mask(data: &mut [u8], key: [u8; 4]) {
    for (idx, byte) in data.iter_mut().enumerate() {
        *byte ^= key[idx % 4];
    }
}

fn next_ws_key() -> String {
    let n = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0u128, |d| d.as_nanos());
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    bytes[8..].copy_from_slice(&(nanos as u64).to_le_bytes());
    STANDARD.encode(bytes)
}

fn next_mask_key() -> [u8; 4] {
    let n = NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0u64, |d| d.subsec_nanos() as u64);
    ((n ^ nanos.wrapping_mul(0x9E37_79B9)) as u32).to_be_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn performs_handshake_and_exchanges_masked_and_fragmented_frames() {
        let (client_stream, mut server_stream) = tokio::io::duplex(4096);

        let server_task = tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            let n = server_stream.read(&mut buf).await.expect("read upgrade");
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains("Upgrade: websocket"));
            server_stream
                .write_all(
                    b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
                )
                .await
                .expect("write 101");

            // Read one masked text frame from client.
            let mut reader = WsReader {
                reader: BufReader::new(server_stream),
            };
            let msg = reader.next_message().await.expect("client text");
            assert_eq!(msg, WsMessage::Text(r#"{"id":1}"#.to_owned()));

            // Write unmasked fragmented text response back to client.
            let mut inner = reader.reader.into_inner();
            // Frame 1: FIN=0, text opcode 0x1, len 3 ("hel")
            inner
                .write_all(&[0x01, 0x03, b'h', b'e', b'l'])
                .await
                .expect("write frag 1");
            // Frame 2: FIN=1, continuation 0x0, len 2 ("lo")
            inner
                .write_all(&[0x80, 0x02, b'l', b'o'])
                .await
                .expect("write frag 2");
        });

        let (mut ws_reader, mut ws_writer) =
            handshake_ws(client_stream, "127.0.0.1:9222", "/devtools/page/1")
                .await
                .expect("handshake succeeds");
        ws_writer.send_text(r#"{"id":1}"#).await.expect("send text");
        let reply = ws_reader.next_message().await.expect("read reply");
        assert_eq!(reply, WsMessage::Text("hello".to_owned()));

        server_task.await.expect("server completes");
    }
}
