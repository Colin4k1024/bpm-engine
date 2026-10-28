//! The stdio run loop: newline-delimited JSON-RPC 2.0 over the process's
//! standard streams (the MCP stdio transport).
//!
//! Reads one message per line from stdin, hands it to
//! [`McpServer::handle_line`], and writes each response to stdout as a
//! single compact line (MCP forbids embedded newlines and anything that is
//! not a valid MCP message on stdout — logs go to stderr). Blank lines are
//! skipped; malformed lines get a JSON-RPC error response and the loop
//! keeps reading; **stdin EOF exits cleanly** (the MCP shutdown signal).
//!
//! # Bounded reading — the line limit is enforced HERE
//!
//! The loop never buffers an unbounded line: it reads one
//! [`AsyncBufReadExt::fill_buf`] chunk at a time into the current frame and
//! declares the frame **oversized** as soon as more than
//! [`MAX_LINE_BYTES`] bytes (plus one `READ_CHUNK`-sized slack) arrive
//! without a newline — a client streaming bytes without delimiters grows
//! this process's memory by at most [`MAX_LINE_BYTES`] + ~128 KiB, never
//! without bound. An oversized frame answers exactly ONE `-32700` error
//! on stdout and then closes the stream: JSON-RPC permits either
//! answering or dropping, and resynchronizing framing after an
//! undelimited overrun is impossible in bounded memory. A frame that
//! arrives WITH its newline but over the message bound is still assembled
//! (inside the same memory bound) and rejected by [`McpServer::handle_line`]
//! with `-32600` — the loop keeps reading; that check is defense in depth
//! for exactly this window. Invalid UTF-8 inside an otherwise well-framed
//! line answers `-32700` and the loop keeps reading, like any malformed
//! line (the delimiter was consumed, so the framing is intact).
//!
//! Messages are processed sequentially in arrival order; a slow REST call
//! (bounded by the configured timeout) delays subsequent messages but
//! never blocks the process indefinitely.

use crate::protocol::{error_line, McpServer, PARSE_ERROR};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Upper bound on one stdin message (guards the process against a runaway
/// client; deploy bodies are far below it). ENFORCED by this read loop
/// (see the module docs) and re-checked by [`McpServer::handle_line`] as
/// defense in depth.
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// One `fill_buf` quantum: the syscall granularity of the bounded read.
/// This is NOT the message bound — the frame cap tolerates this much
/// slack past [`MAX_LINE_BYTES`] before declaring a frame oversized, so a
/// delimiter arriving just past the bound is still delivered and rejected
/// at the message layer instead of tearing down the connection.
const READ_CHUNK: usize = 64 * 1024;

/// What one bounded read produced.
enum Frame {
    /// A newline-terminated line (or the EOF tail of one), including the
    /// trailing delimiter. `handle_line` trims it and enforces the
    /// message bound.
    Line(Vec<u8>),
    /// Clean EOF at a frame boundary: the client closed stdin.
    Eof,
    /// More than [`MAX_LINE_BYTES`] (+ one chunk) of bytes without a
    /// newline: an undelimited overrun. The payload is the number of
    /// bytes buffered when the cap fired.
    Oversized(usize),
}

/// Read one frame, BOUNDED: the buffer never grows past
/// [`MAX_LINE_BYTES`] + one `READ_CHUNK` + one internal quantum before
/// the frame is declared oversized.
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> std::io::Result<Frame> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            // EOF: an empty buffer is the clean shutdown signal; a
            // non-empty one is the final partial line — still a message
            // (`BufRead::lines` semantics).
            return Ok(if buf.is_empty() {
                Frame::Eof
            } else {
                Frame::Line(buf)
            });
        }
        if let Some(pos) = available.iter().position(|&b| b == b'\n') {
            buf.extend_from_slice(&available[..=pos]);
            reader.consume(pos + 1);
            return Ok(Frame::Line(buf));
        }
        let chunk = available.len();
        buf.extend_from_slice(available);
        reader.consume(chunk);
        if buf.len() > MAX_LINE_BYTES + READ_CHUNK {
            return Ok(Frame::Oversized(buf.len()));
        }
    }
}

/// Run the MCP server over stdin/stdout until stdin reaches EOF.
pub async fn run_stdio(server: McpServer) -> anyhow::Result<()> {
    run_over(tokio::io::stdin(), tokio::io::stdout(), server).await
}

/// Run the MCP loop over arbitrary read/write streams — the stdio entry
/// point, and the testable core: the in-process suite (`tests/stdio.rs`)
/// drives it over tokio duplex pipes, and the subprocess suite
/// (`tests/stdio_process.rs`) covers the real stdio wiring of the binary.
pub async fn run_over<R, W>(stdin: R, stdout: W, server: McpServer) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::with_capacity(READ_CHUNK, stdin);
    let mut stdout = stdout;

    loop {
        let frame = match read_frame(&mut reader).await {
            Ok(frame) => frame,
            // Unreadable stdin (an I/O error): there is no way to keep
            // speaking the protocol; surface on stderr and stop.
            Err(e) => anyhow::bail!("reading stdin failed: {e}"),
        };
        let line = match frame {
            Frame::Eof => {
                // The client closed stdin — the graceful shutdown signal.
                break;
            }
            Frame::Oversized(bytes) => {
                // One error, then close the stream (see the module docs):
                // the frame has no delimiter to resynchronize on.
                let reply = error_line(
                    PARSE_ERROR,
                    format!(
                        "oversized frame: {bytes} bytes without a newline exceed the \
                         {MAX_LINE_BYTES} byte message limit; closing the connection"
                    ),
                );
                let _ = write_reply(&mut stdout, &reply).await?;
                break;
            }
            Frame::Line(bytes) => match String::from_utf8(bytes) {
                // from_utf8 CONSUMES the frame's buffer (no copy when
                // valid), giving an owned line that outlives this match.
                Ok(line) => line,
                Err(e) => {
                    // Framing is intact (the delimiter was consumed); the
                    // payload is undecodable — a malformed line like any
                    // other, so answer -32700 and keep reading.
                    let reply = error_line(
                        PARSE_ERROR,
                        format!(
                            "parse error: message is not valid UTF-8 ({})",
                            e.utf8_error()
                        ),
                    );
                    if !write_reply(&mut stdout, &reply).await? {
                        break; // broken pipe: the client is gone
                    }
                    continue;
                }
            },
        };
        if let Some(reply) = server.handle_line(&line).await {
            if !write_reply(&mut stdout, &reply).await? {
                // The client stopped reading and closed stdout: it is
                // gone — exit cleanly instead of erroring.
                break;
            }
        }
    }
    Ok(())
}

/// Write one response and report whether the stream is still alive.
/// `Ok(false)` = broken pipe: the client stopped reading and closed
/// stdout — the caller exits cleanly instead of spinning on a dead pipe.
async fn write_reply<W: AsyncWrite + Unpin>(stdout: &mut W, reply: &str) -> anyhow::Result<bool> {
    match write_line(stdout, reply).await {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(false),
        Err(e) => anyhow::bail!("writing stdout failed: {e}"),
    }
}

/// Write one response line (compact JSON + the newline delimiter).
async fn write_line<W: AsyncWrite + Unpin>(stdout: &mut W, reply: &str) -> std::io::Result<()> {
    stdout.write_all(reply.as_bytes()).await?;
    stdout.write_all(b"\n").await?;
    stdout.flush().await
}
