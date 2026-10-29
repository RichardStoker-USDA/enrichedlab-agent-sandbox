use std::{collections::HashSet, fs, net::SocketAddr, path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{copy, copy_bidirectional},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::Semaphore,
    task::JoinSet,
    time::timeout,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RelayConfig {
    pub listeners: Vec<RelayListenerConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RelayListenerConfig {
    pub name: String,
    pub listen: SocketAddr,
    pub upstream: String,
    pub max_connections: usize,
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout_seconds: u64,
    #[serde(default)]
    pub connector: Connector,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Connector {
    #[default]
    Native,
    SystemNc,
}

impl RelayConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let body = fs::read(path)
            .with_context(|| format!("failed to read relay config {}", path.display()))?;
        let config: Self = serde_json::from_slice(&body)
            .with_context(|| format!("invalid relay config {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.listeners.is_empty() {
            bail!("relay config needs at least one listener");
        }
        let mut names = HashSet::new();
        let mut addresses = HashSet::new();
        for listener in &self.listeners {
            if !listener.listen.ip().is_loopback() {
                bail!(
                    "relay {} must listen on a loopback address, got {}",
                    listener.name,
                    listener.listen
                );
            }
            if listener.listen.port() == 0 || !addresses.insert(listener.listen) {
                bail!(
                    "relay listener address is zero or duplicated: {}",
                    listener.listen
                );
            }
            if listener.name.is_empty()
                || !listener
                    .name
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                || !names.insert(listener.name.clone())
            {
                bail!("relay names must be unique lowercase DNS labels");
            }
            if listener.max_connections == 0 || listener.max_connections > 128 {
                bail!(
                    "relay {} max_connections must be between 1 and 128",
                    listener.name
                );
            }
            validate_upstream(&listener.upstream)?;
            if listener.connect_timeout_seconds == 0 || listener.connect_timeout_seconds > 120 {
                bail!(
                    "relay {} connect timeout must be between 1 and 120 seconds",
                    listener.name
                );
            }
        }
        Ok(())
    }
}

pub async fn serve(config: RelayConfig) -> Result<()> {
    let mut tasks = JoinSet::new();
    for listener in config.listeners {
        tasks.spawn(serve_listener(listener));
    }

    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal.context("failed to listen for interrupt")?;
        }
        _ = terminate.recv() => {}
        result = tasks.join_next() => {
            match result {
                Some(Ok(Ok(()))) => bail!("relay listener stopped unexpectedly"),
                Some(Ok(Err(error))) => return Err(error),
                Some(Err(error)) => return Err(error.into()),
                None => bail!("relay has no running listeners"),
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

async fn serve_listener(config: RelayListenerConfig) -> Result<()> {
    let listener = TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("failed to bind {} on {}", config.name, config.listen))?;
    eprintln!(
        "relay {} listening on {} -> {} (max {})",
        config.name, config.listen, config.upstream, config.max_connections
    );
    let permits = Arc::new(Semaphore::new(config.max_connections));

    loop {
        let permit = Arc::clone(&permits).acquire_owned().await?;
        let (mut downstream, peer) = listener.accept().await?;
        let route = config.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let result = match route.connector {
                Connector::Native => relay_native(&route, &mut downstream).await,
                Connector::SystemNc => relay_system_nc(&route, downstream).await,
            };
            if let Err(error) = result {
                eprintln!("relay {} connection {} failed: {error}", route.name, peer);
            }
        });
    }
}

async fn relay_native(config: &RelayListenerConfig, downstream: &mut TcpStream) -> Result<()> {
    let connect = TcpStream::connect(&config.upstream);
    let mut upstream = timeout(Duration::from_secs(config.connect_timeout_seconds), connect)
        .await
        .context("upstream connect timed out")??;
    let _ = downstream.set_nodelay(true);
    let _ = upstream.set_nodelay(true);
    copy_bidirectional(downstream, &mut upstream).await?;
    Ok(())
}

async fn relay_system_nc(config: &RelayListenerConfig, mut downstream: TcpStream) -> Result<()> {
    let (host, port) = split_upstream(&config.upstream)?;
    let mut child = Command::new("/usr/bin/nc")
        .arg("-G")
        .arg(config.connect_timeout_seconds.to_string())
        .arg(host)
        .arg(port)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("failed to start /usr/bin/nc")?;
    let mut child_stdin = child.stdin.take().context("nc stdin is unavailable")?;
    let mut child_stdout = child.stdout.take().context("nc stdout is unavailable")?;
    let (mut downstream_read, mut downstream_write) = downstream.split();
    let upload = copy(&mut downstream_read, &mut child_stdin);
    let download = copy(&mut child_stdout, &mut downstream_write);
    let result = tokio::try_join!(upload, download);
    drop(child_stdin);
    let _ = child.start_kill();
    let _ = child.wait().await;
    result?;
    Ok(())
}

fn validate_upstream(value: &str) -> Result<()> {
    if value.contains('/') || value.contains('@') || value.chars().any(char::is_whitespace) {
        bail!("relay upstream must be host:port without credentials or a URL: {value}");
    }
    let (host, port) = split_upstream(value)?;
    if host.is_empty() || port.parse::<u16>().ok().filter(|port| *port > 0).is_none() {
        bail!("invalid relay upstream: {value}");
    }
    Ok(())
}

fn split_upstream(value: &str) -> Result<(&str, &str)> {
    value
        .rsplit_once(':')
        .context("relay upstream must be host:port")
}

const fn default_connect_timeout() -> u64 {
    10
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use super::{Connector, RelayConfig, RelayListenerConfig};

    fn listener() -> RelayListenerConfig {
        RelayListenerConfig {
            name: "route-a".into(),
            listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 38000),
            upstream: "192.0.2.9:30800".into(),
            max_connections: 4,
            connect_timeout_seconds: 10,
            connector: Connector::Native,
        }
    }

    #[test]
    fn accepts_loopback_listener() {
        assert!(
            RelayConfig {
                listeners: vec![listener()]
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn rejects_exposed_listener() {
        let mut item = listener();
        item.listen = "0.0.0.0:38000".parse().unwrap();
        assert!(
            RelayConfig {
                listeners: vec![item]
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn rejects_credentialed_or_url_upstreams() {
        for upstream in ["http://10.0.0.1:80", "user@host:80", "host"] {
            let mut item = listener();
            item.upstream = upstream.into();
            assert!(
                RelayConfig {
                    listeners: vec![item]
                }
                .validate()
                .is_err()
            );
        }
    }
}
