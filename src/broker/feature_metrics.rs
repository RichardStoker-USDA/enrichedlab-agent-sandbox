//! Bounded Prometheus state and the optional feature-metrics listener.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const LAB_LIMIT: usize = 64;
const BUCKETS: [f64; 11] = [0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 120.0];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct EventKey {
    module: &'static str,
    feature: &'static str,
    outcome: &'static str,
    error_code: &'static str,
    lab: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct DurationKey {
    module: &'static str,
    feature: &'static str,
    outcome: &'static str,
}

#[derive(Clone, Debug)]
struct Histogram {
    buckets: [u64; BUCKETS.len()],
    count: u64,
    sum: f64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: [0; BUCKETS.len()],
            count: 0,
            sum: 0.0,
        }
    }
}

#[derive(Default)]
struct State {
    labs: BTreeSet<String>,
    events: BTreeMap<EventKey, u64>,
    durations: BTreeMap<DurationKey, Histogram>,
}

/// Metric labels enter only through fixed enums in `feature_events`; lab IDs are
/// syntax checked and capped to keep the remaining dynamic label bounded.
#[derive(Default)]
pub struct FeatureMetrics {
    state: Mutex<State>,
}

impl FeatureMetrics {
    pub fn record(
        &self,
        module: &'static str,
        feature: &'static str,
        outcome: &'static str,
        error_code: &'static str,
        lab: &str,
        duration_seconds: f64,
    ) -> String {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => return "other".to_owned(),
        };
        let lab = bounded_lab(&mut state.labs, lab);
        *state
            .events
            .entry(EventKey {
                module,
                feature,
                outcome,
                error_code,
                lab: lab.clone(),
            })
            .or_default() += 1;
        let histogram = state
            .durations
            .entry(DurationKey {
                module,
                feature,
                outcome,
            })
            .or_default();
        histogram.count += 1;
        histogram.sum += duration_seconds.max(0.0);
        for (index, upper) in BUCKETS.iter().enumerate() {
            if duration_seconds <= *upper {
                histogram.buckets[index] += 1;
            }
        }
        lab
    }

    pub fn render(&self) -> String {
        let Ok(state) = self.state.lock() else {
            return String::new();
        };
        let mut out = String::from(
            "# HELP enrichedlab_feature_events_total Completed feature attempts.\n\
# TYPE enrichedlab_feature_events_total counter\n",
        );
        for (key, count) in &state.events {
            let _ = writeln!(
                out,
                "enrichedlab_feature_events_total{{module=\"{}\",feature=\"{}\",outcome=\"{}\",error_code=\"{}\",lab=\"{}\"}} {}",
                key.module, key.feature, key.outcome, key.error_code, key.lab, count
            );
        }
        out.push_str(
            "# HELP enrichedlab_feature_duration_seconds Completed feature-attempt duration.\n\
# TYPE enrichedlab_feature_duration_seconds histogram\n",
        );
        for (key, histogram) in &state.durations {
            for (index, upper) in BUCKETS.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "enrichedlab_feature_duration_seconds_bucket{{module=\"{}\",feature=\"{}\",outcome=\"{}\",le=\"{}\"}} {}",
                    key.module, key.feature, key.outcome, upper, histogram.buckets[index]
                );
            }
            let labels = format!(
                "module=\"{}\",feature=\"{}\",outcome=\"{}\"",
                key.module, key.feature, key.outcome
            );
            let _ = writeln!(
                out,
                "enrichedlab_feature_duration_seconds_bucket{{{labels},le=\"+Inf\"}} {}",
                histogram.count
            );
            let _ = writeln!(
                out,
                "enrichedlab_feature_duration_seconds_sum{{{labels}}} {}",
                histogram.sum
            );
            let _ = writeln!(
                out,
                "enrichedlab_feature_duration_seconds_count{{{labels}}} {}",
                histogram.count
            );
        }
        out
    }
}

fn bounded_lab(seen: &mut BTreeSet<String>, lab: &str) -> String {
    let valid = !lab.is_empty()
        && lab.len() <= 63
        && lab.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'_' | b'-'))
        });
    if !valid {
        return "other".to_owned();
    }
    if seen.contains(lab) || seen.len() < LAB_LIMIT {
        seen.insert(lab.to_owned());
        lab.to_owned()
    } else {
        "other".to_owned()
    }
}

pub async fn listen(bind: SocketAddr, metrics: Arc<FeatureMetrics>) -> Result<()> {
    let listener = TcpListener::bind(bind).await?;
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
            let metrics = metrics.clone();
            tokio::spawn(async move {
                let _ = serve(stream, metrics).await;
            });
        }
    });
    Ok(())
}

async fn serve(mut stream: TcpStream, metrics: Arc<FeatureMetrics>) -> std::io::Result<()> {
    let mut request = [0_u8; 1024];
    let read = stream.read(&mut request).await?;
    let metrics_request = request[..read].starts_with(b"GET /metrics HTTP/1.0\r\n")
        || request[..read].starts_with(b"GET /metrics HTTP/1.1\r\n");
    let (status, body) = if metrics_request {
        ("200 OK", metrics.render())
    } else {
        ("404 Not Found", String::new())
    };
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labs_are_syntax_checked_and_capped() {
        let metrics = FeatureMetrics::default();
        for index in 0..LAB_LIMIT + 5 {
            metrics.record(
                "research-broker",
                "broker.query",
                "success",
                "",
                &format!("lab_{index}"),
                0.1,
            );
        }
        let text = metrics.render();
        assert!(text.contains("lab=\"other\""));
        assert!(!text.contains("private lab name"));
        let distinct = text
            .lines()
            .filter(|line| line.starts_with("enrichedlab_feature_events_total{"))
            .count();
        assert_eq!(distinct, LAB_LIMIT + 1);
    }
}
