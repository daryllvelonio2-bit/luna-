//! Remote audio daemon and binary framing protocol (`micserve` / `micwire`).
//! Reconstructed from binary analysis of agy audio subsystem (`google3/third_party/jetski/cli/audio`).

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use harness_core::HarnessError;

#[allow(dead_code)]
pub const DEFAULT_MIC_SERVE_ADDR: &str = "127.0.0.1:4713";
#[allow(dead_code)]
pub const DEFAULT_SAMPLE_RATE: u32 = 16_000;

/// Check if a remote mic server daemon is configured via environment variables.
#[allow(dead_code)]
pub fn is_mic_server_configured() -> Option<String> {
    for var in ["MIC_SERVER", "MICS_SERVER", "MIC_ENGINE_ADDR", "AGY_MIC_SERVER"] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// Construct `SPEAKER rate={rate}\n` handshake line.
#[allow(dead_code)]
pub fn speaker_hello(rate: u32) -> String {
    format!("SPEAKER rate={rate}\n")
}

/// Parse `SPEAKER rate={rate}` handshake line.
pub fn parse_speaker_hello(line: &str) -> Option<u32> {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("SPEAKER rate=") {
        rest.parse::<u32>().ok()
    } else {
        None
    }
}

/// Write a length-prefixed binary frame (4-byte big-endian length prefix).
#[allow(dead_code)]
pub fn write_frame<W: Write>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    let len = data.len() as u32;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(data)?;
    w.flush()
}

/// Read a length-prefixed binary frame (4-byte big-endian length prefix).
#[allow(dead_code)]
pub fn read_frame<R: Read>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 10 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Frame length {len} exceeds 10MB limit"),
        ));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Run the `micserve` audio daemon to forward local microphone and speaker over TCP.
pub async fn run_mic_serve(addr: &str, cancel: CancellationToken) -> Result<(), HarnessError> {
    let bind_addr: SocketAddr = addr.parse().map_err(|e| {
        HarnessError::VoiceError(format!("Invalid mic-serve address '{addr}': {e}"))
    })?;

    let listener = tokio::net::TcpListener::bind(bind_addr).await.map_err(|e| {
        HarnessError::VoiceError(format!("Failed to bind mic-serve on '{bind_addr}': {e}"))
    })?;

    println!("🎙 Serving microphone and speaker on {bind_addr}");

    let is_busy = Arc::new(AtomicBool::new(false));

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                println!("• mic-serve daemon stopping");
                break;
            }
            conn_res = listener.accept() => {
                match conn_res {
                    Ok((stream, peer)) => {
                        let is_busy_conn = is_busy.clone();
                        let cancel_conn = cancel.clone();
                        tokio::spawn(async move {
                            if let Err(e) = handle_client_conn(stream, peer, is_busy_conn, cancel_conn).await {
                                tracing::debug!("mic-serve client {peer} closed: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!("mic-serve accept error: {e}");
                    }
                }
            }
        }
    }

    Ok(())
}

async fn handle_client_conn(
    mut stream: tokio::net::TcpStream,
    peer: SocketAddr,
    _is_busy: Arc<AtomicBool>,
    cancel: CancellationToken,
) -> Result<(), std::io::Error> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let (reader, mut writer) = stream.split();
    let mut buf_reader = BufReader::new(reader);

    let mut line = String::new();
    let n = buf_reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(());
    }

    if let Some(rate) = parse_speaker_hello(&line) {
        tracing::info!("mic-serve: client {peer} connected for speaker streaming ({rate}Hz)");
        writer.write_all(b"OK\n").await?;

        // Stream audio frames to local speaker process
        let player_tool = super::playback::select_player_tool().unwrap_or_else(|| "pw-cat".to_string());
        let mut player = super::playback::NativePlayerProcess::spawn(&player_tool, rate)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        while !cancel.is_cancelled() {
            let mut len_buf = [0u8; 4];
            if buf_reader.read_exact(&mut len_buf).await.is_err() {
                break;
            }
            let len = u32::from_be_bytes(len_buf) as usize;
            if len > 5 * 1024 * 1024 {
                break;
            }
            let mut data = vec![0u8; len];
            if buf_reader.read_exact(&mut data).await.is_err() {
                break;
            }
            #[allow(clippy::chunks_exact_to_as_chunks)]
            let samples: Vec<i16> = data
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            let _ = player.write_samples(&samples);
        }
        player.kill();
    } else {
        writer.write_all(b"ERR unknown command\n").await?;
    }

    Ok(())
}
