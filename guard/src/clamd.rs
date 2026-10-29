//! The ClamAV daemon client (L1): `INSTREAM` for a scan and `VERSION` for the engine and signature dates. The daemon
//! runs as its own Deployment; it receives bytes over TCP and has no access to the guard's volume.
use anyhow::{Context, Result, bail};
use std::time::Duration;
use time::OffsetDateTime;
use time::macros::format_description;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const CHUNK: usize = 64 * 1024;
const REPLY_LIMIT: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    Found(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub text: String,
    pub signatures: OffsetDateTime,
}

async fn exchange(
    address: &str,
    command: &[u8],
    body: Option<&[u8]>,
    timeout: Duration,
) -> Result<String> {
    let work = async {
        let mut stream = TcpStream::connect(address)
            .await
            .context("scanner unreachable")?;
        stream.write_all(command).await?;
        if let Some(body) = body {
            for chunk in body.chunks(CHUNK) {
                stream
                    .write_all(&(chunk.len() as u32).to_be_bytes())
                    .await?;
                stream.write_all(chunk).await?;
            }
            stream.write_all(&0u32.to_be_bytes()).await?;
        }
        stream.flush().await?;
        let mut reply = Vec::new();
        let mut buf = [0u8; 512];
        loop {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            reply.extend_from_slice(&buf[..n]);
            if reply.contains(&0) || reply.len() > REPLY_LIMIT {
                break;
            }
        }
        let end = reply.iter().position(|b| *b == 0).unwrap_or(reply.len());
        Ok::<String, anyhow::Error>(String::from_utf8_lossy(&reply[..end]).trim().to_string())
    };
    tokio::time::timeout(timeout, work)
        .await
        .context("scanner timed out")?
}

pub fn parse_scan(reply: &str) -> Result<Verdict> {
    let body = reply
        .strip_prefix("stream:")
        .map(str::trim)
        .unwrap_or(reply);
    if body == "OK" {
        return Ok(Verdict::Clean);
    }
    if let Some(name) = body.strip_suffix(" FOUND") {
        let name: String = name
            .chars()
            .filter(|c| c.is_ascii_graphic())
            .take(128)
            .collect();
        return Ok(Verdict::Found(name));
    }
    bail!(
        "scanner error: {}",
        body.chars().take(120).collect::<String>()
    )
}

pub async fn scan(address: &str, bytes: &[u8], timeout: Duration) -> Result<Verdict> {
    parse_scan(&exchange(address, b"zINSTREAM\0", Some(bytes), timeout).await?)
}

/// `ClamAV 1.4.3/27788/Sun Oct  5 08:30:00 2026`: the engine, the daily signature version and its build time.
pub fn parse_version(reply: &str) -> Result<Version> {
    let mut parts = reply.splitn(3, '/');
    let engine = parts.next().unwrap_or("");
    let _daily = parts.next().context("no signature version")?;
    let date = parts.next().context("no signature date")?;
    if !engine.starts_with("ClamAV ") {
        bail!("not a ClamAV version reply");
    }
    let compact = date.split_whitespace().collect::<Vec<_>>().join(" ");
    let format = format_description!(
        "[weekday repr:short] [month repr:short] [day padding:none] [hour]:[minute]:[second] [year]"
    );
    let signatures = time::PrimitiveDateTime::parse(&compact, format)
        .context("signature date unreadable")?
        .assume_utc();
    Ok(Version {
        text: reply.to_string(),
        signatures,
    })
}

pub async fn version(address: &str, timeout: Duration) -> Result<Version> {
    parse_version(&exchange(address, b"zVERSION\0", None, timeout).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{EICAR, fake_daemon};

    #[test]
    fn parses_replies() {
        assert_eq!(parse_scan("stream: OK").unwrap(), Verdict::Clean);
        assert_eq!(
            parse_scan("stream: Win.Test.EICAR_HDB-1 FOUND").unwrap(),
            Verdict::Found("Win.Test.EICAR_HDB-1".into())
        );
        assert!(parse_scan("INSTREAM size limit exceeded. ERROR").is_err());
        let v = parse_version("ClamAV 1.4.3/27788/Sun Oct  5 08:30:00 2026").unwrap();
        assert_eq!(v.signatures.year(), 2026);
        assert_eq!(v.signatures.day(), 5);
        assert!(parse_version("nonsense").is_err());
    }

    #[tokio::test]
    async fn scans_over_the_stream_protocol() {
        let address = fake_daemon("Sun Oct  5 08:30:00 2026").await;
        let timeout = Duration::from_secs(5);
        assert_eq!(
            scan(&address, b"%PDF-1.7 clean", timeout).await.unwrap(),
            Verdict::Clean
        );
        let mut infected = vec![b'x'; 200_000];
        infected.extend_from_slice(EICAR);
        assert_eq!(
            scan(&address, &infected, timeout).await.unwrap(),
            Verdict::Found("Eicar-Test-Signature".into())
        );
        assert_eq!(
            version(&address, timeout).await.unwrap().signatures.month(),
            time::Month::October
        );
        assert!(scan("127.0.0.1:9", b"x", timeout).await.is_err());
    }
}
