//! Fixed-shape feature-completion events for the platform telemetry stream.
//!
//! The public API is typed on purpose: callers cannot attach query text, model
//! output, paths, upstream errors, or arbitrary labels.

use std::{
    env,
    ffi::OsString,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Map, Value};
use sha2::Sha256;
use uuid::Uuid;

use super::{feature_metrics::FeatureMetrics, library::private_bytes, model::ClaimedTask};

const KEY_FILE_ENV: &str = "AGENT_SANDBOX_FEATURE_PSEUDONYM_KEY_FILE";
const METRICS_BIND_ENV: &str = "AGENT_SANDBOX_FEATURE_METRICS_BIND";
const RELEASE_ENV: &str = "AGENT_SANDBOX_RELEASE";
static EVENTS: OnceLock<Arc<FeatureEvents>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Feature {
    Query,
    Analysis,
    Figure,
    Dataset,
    Literature,
    Package,
    Campaign,
    CoordinatorTaskStep,
}

impl Feature {
    pub const ALL: [Self; 8] = [
        Self::Query,
        Self::Analysis,
        Self::Figure,
        Self::Dataset,
        Self::Literature,
        Self::Package,
        Self::Campaign,
        Self::CoordinatorTaskStep,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Query => "broker.query",
            Self::Analysis => "broker.analysis",
            Self::Figure => "broker.figure",
            Self::Dataset => "broker.dataset",
            Self::Literature => "broker.literature",
            Self::Package => "broker.package",
            Self::Campaign => "broker.campaign",
            Self::CoordinatorTaskStep => "coordinator.task.step",
        }
    }

    const fn module(self) -> &'static str {
        match self {
            Self::Literature | Self::Campaign | Self::CoordinatorTaskStep => "research-coordinator",
            _ => "research-broker",
        }
    }

    const fn event_name(self) -> &'static str {
        match self {
            Self::Query => "broker.query.completed",
            Self::Analysis => "broker.analysis.completed",
            Self::Figure => "broker.figure.completed",
            Self::Dataset => "broker.dataset.completed",
            Self::Literature => "broker.literature.completed",
            Self::Package => "broker.package.completed",
            Self::Campaign => "broker.campaign.completed",
            Self::CoordinatorTaskStep => "coordinator.task.step.completed",
        }
    }

    const fn failure_code(self) -> ErrorCode {
        match self {
            Self::Query => ErrorCode::QueryFailed,
            Self::Analysis => ErrorCode::AnalysisFailed,
            Self::Figure => ErrorCode::FigureFailed,
            Self::Dataset => ErrorCode::DatasetFailed,
            Self::Literature => ErrorCode::LiteratureUnavailable,
            Self::Package => ErrorCode::PackageFailed,
            Self::Campaign => ErrorCode::CampaignFailed,
            Self::CoordinatorTaskStep => ErrorCode::TaskStepFailed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Success,
    Failure,
    Refused,
    Cancelled,
    Timeout,
}

impl Outcome {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Refused => "refused",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrincipalKind {
    Person,
    Service,
    Agent,
}

impl PrincipalKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Service => "service",
            Self::Agent => "agent",
        }
    }
}

/// Broker-owned codes only. There is no string escape hatch for exception or
/// upstream response text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCode {
    MissingRequest,
    QueryRefused,
    QueryFailed,
    AnalysisFailed,
    FigureFailed,
    DatasetFailed,
    LiteratureUnavailable,
    PackageFailed,
    PackageChangedWhileStreaming,
    CampaignFailed,
    TaskStepFailed,
    Cancelled,
    Timeout,
    InternalFailure,
}

impl ErrorCode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::MissingRequest => "missing_request",
            Self::QueryRefused => "query_refused",
            Self::QueryFailed => "query_failed",
            Self::AnalysisFailed => "analysis_failed",
            Self::FigureFailed => "figure_failed",
            Self::DatasetFailed => "dataset_failed",
            Self::LiteratureUnavailable => "literature_unavailable",
            Self::PackageFailed => "package_failed",
            Self::PackageChangedWhileStreaming => "package_changed_while_streaming",
            Self::CampaignFailed => "campaign_failed",
            Self::TaskStepFailed => "task_step_failed",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::InternalFailure => "internal_failure",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraceContext {
    pub trace_id: String,
    pub span_id: String,
}

impl TraceContext {
    pub fn new() -> Self {
        let trace_id = Uuid::new_v4().simple().to_string();
        let span_id = Uuid::new_v4().simple().to_string()[..16].to_owned();
        Self { trace_id, span_id }
    }

    pub fn from_traceparent(value: Option<&str>) -> Self {
        let Some(value) = value.filter(|value| value.len() == 55) else {
            return Self::new();
        };
        let bytes = value.as_bytes();
        let valid = &bytes[0..3] == b"00-"
            && bytes[35] == b'-'
            && bytes[52] == b'-'
            && bytes[3..35].iter().all(u8::is_ascii_hexdigit)
            && bytes[36..52].iter().all(u8::is_ascii_hexdigit)
            && bytes[53..55].iter().all(u8::is_ascii_hexdigit)
            && bytes[3..35].iter().any(|byte| *byte != b'0')
            && bytes[36..52].iter().any(|byte| *byte != b'0');
        if valid {
            Self {
                trace_id: value[3..35].to_ascii_lowercase(),
                span_id: value[36..52].to_ascii_lowercase(),
            }
        } else {
            Self::new()
        }
    }
}

impl Default for TraceContext {
    fn default() -> Self {
        Self::new()
    }
}

pub struct CompletedAttempt<'a> {
    pub feature: Feature,
    pub outcome: Outcome,
    pub error: Option<ErrorCode>,
    pub duration: Duration,
    pub attempt: u32,
    pub principal_id: Uuid,
    pub principal_kind: PrincipalKind,
    pub lab: &'a str,
    pub trace: TraceContext,
    pub agent_run_id: Option<Uuid>,
    pub items_returned: u64,
    pub items_refused: u64,
}

/// A failure-default guard for request paths that use `?`. Once the caller has
/// authenticated and selected a lab, every later return emits exactly one event.
pub struct AttemptGuard {
    feature: Feature,
    started: Instant,
    attempt: u32,
    principal_id: Uuid,
    principal_kind: PrincipalKind,
    lab: String,
    trace: TraceContext,
    agent_run_id: Option<Uuid>,
    active: bool,
}

impl AttemptGuard {
    pub fn for_task(feature: Feature, task: &ClaimedTask) -> Self {
        Self::new(
            feature,
            task.attempt.max(0) as u32,
            task.principal_id,
            PrincipalKind::Agent,
            task.group_key.clone(),
            TraceContext::new(),
            Some(task.id),
        )
    }

    pub fn new(
        feature: Feature,
        attempt: u32,
        principal_id: Uuid,
        principal_kind: PrincipalKind,
        lab: impl Into<String>,
        trace: TraceContext,
        agent_run_id: Option<Uuid>,
    ) -> Self {
        Self {
            feature,
            started: Instant::now(),
            attempt,
            principal_id,
            principal_kind,
            lab: lab.into(),
            trace,
            agent_run_id,
            active: true,
        }
    }

    pub fn finish(
        mut self,
        outcome: Outcome,
        error: Option<ErrorCode>,
        returned: u64,
        refused: u64,
    ) {
        self.active = false;
        completed(CompletedAttempt {
            feature: self.feature,
            outcome,
            error,
            duration: self.started.elapsed(),
            attempt: self.attempt,
            principal_id: self.principal_id,
            principal_kind: self.principal_kind,
            lab: &self.lab,
            trace: self.trace.clone(),
            agent_run_id: self.agent_run_id,
            items_returned: returned,
            items_refused: refused,
        });
    }
}

impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if self.active {
            completed(CompletedAttempt {
                feature: self.feature,
                outcome: Outcome::Failure,
                error: Some(self.feature.failure_code()),
                duration: self.started.elapsed(),
                attempt: self.attempt,
                principal_id: self.principal_id,
                principal_kind: self.principal_kind,
                lab: &self.lab,
                trace: self.trace.clone(),
                agent_run_id: self.agent_run_id,
                items_returned: 0,
                items_refused: 1,
            });
        }
    }
}

struct FeatureEvents {
    key: Vec<u8>,
    release: String,
    metrics: Arc<FeatureMetrics>,
}

#[derive(Debug)]
struct InstallConfiguration {
    key_file: PathBuf,
    metrics_bind: Option<SocketAddr>,
}

fn install_configuration(
    metrics_bind: Option<OsString>,
    key_file: Option<OsString>,
) -> Result<InstallConfiguration> {
    let metrics_bind = metrics_bind
        .map(|value| value.to_string_lossy().parse::<SocketAddr>())
        .transpose()?;
    let key_file = key_file
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("feature event producer requires a pseudonym key file"))?;
    Ok(InstallConfiguration {
        key_file,
        metrics_bind,
    })
}

pub async fn install() -> Result<()> {
    let configuration =
        install_configuration(env::var_os(METRICS_BIND_ENV), env::var_os(KEY_FILE_ENV))?;
    let key = private_bytes(&configuration.key_file, 4096)?;
    if key.len() < 32 {
        bail!("feature pseudonym key is too short")
    }
    let release = env::var(RELEASE_ENV).unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_owned());
    if release.is_empty()
        || release.len() > 64
        || !release
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        bail!("feature telemetry release is invalid")
    }
    let metrics = Arc::new(FeatureMetrics::default());
    if let Some(bind) = configuration.metrics_bind {
        super::feature_metrics::listen(bind, metrics.clone()).await?;
    }
    EVENTS
        .set(Arc::new(FeatureEvents {
            key,
            release,
            metrics,
        }))
        .map_err(|_| anyhow::anyhow!("feature telemetry already installed"))
}

pub fn completed(attempt: CompletedAttempt<'_>) {
    if let Some(events) = EVENTS.get() {
        events.completed(attempt);
    }
}

impl FeatureEvents {
    fn pseudonym(&self, principal: Uuid) -> String {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.key)
            .expect("validated HMAC-SHA256 key");
        mac.update(principal.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    fn completed(&self, attempt: CompletedAttempt<'_>) {
        let event = self.event_value(&attempt);
        if let Ok(line) = serde_json::to_string(&event) {
            eprintln!("{line}");
        }
    }

    fn event_value(&self, attempt: &CompletedAttempt<'_>) -> Value {
        let outcome = attempt.outcome.as_str();
        let error = if attempt.outcome == Outcome::Success {
            ""
        } else {
            attempt.error.unwrap_or(ErrorCode::InternalFailure).as_str()
        };
        let lab = self.metrics.record(
            attempt.feature.module(),
            attempt.feature.id(),
            outcome,
            error,
            attempt.lab,
            attempt.duration.as_secs_f64(),
        );
        let mut event = Map::new();
        event.insert("event.name".into(), attempt.feature.event_name().into());
        event.insert("event.outcome".into(), outcome.into());
        event.insert("error.code".into(), error.into());
        event.insert(
            "duration_ms".into(),
            duration_millis(attempt.duration).into(),
        );
        event.insert("attempt".into(), u64::from(attempt.attempt).into());
        event.insert(
            "principal.pseudonym".into(),
            self.pseudonym(attempt.principal_id).into(),
        );
        event.insert(
            "principal.kind".into(),
            attempt.principal_kind.as_str().into(),
        );
        event.insert("lab".into(), lab.into());
        event.insert("module".into(), attempt.feature.module().into());
        event.insert("feature.id".into(), attempt.feature.id().into());
        event.insert("release".into(), self.release.clone().into());
        event.insert("trace_id".into(), attempt.trace.trace_id.clone().into());
        event.insert("span_id".into(), attempt.trace.span_id.clone().into());
        if attempt.principal_kind == PrincipalKind::Agent
            && let Some(run) = attempt.agent_run_id
        {
            event.insert("agent.run_id".into(), run.to_string().into());
        }
        event.insert("items.returned".into(), attempt.items_returned.into());
        event.insert("items.refused".into(), attempt.items_refused.into());
        Value::Object(event)
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(key: Vec<u8>) -> FeatureEvents {
        FeatureEvents {
            key,
            release: "r4-test".into(),
            metrics: Arc::new(FeatureMetrics::default()),
        }
    }

    fn value(events: &FeatureEvents, feature: Feature, lab: &str) -> Value {
        let attempt = CompletedAttempt {
            feature,
            outcome: Outcome::Success,
            error: None,
            duration: Duration::from_millis(12),
            attempt: 0,
            principal_id: Uuid::from_u128(1),
            principal_kind: PrincipalKind::Agent,
            lab,
            trace: TraceContext {
                trace_id: "1".repeat(32),
                span_id: "2".repeat(16),
            },
            agent_run_id: Some(Uuid::from_u128(2)),
            items_returned: 3,
            items_refused: 0,
        };
        events.event_value(&attempt)
    }

    #[test]
    fn event_fields_and_types_match_the_console_contract() {
        let telemetry = events(vec![7; 32]);
        let event = value(&telemetry, Feature::Query, "lab_one");
        let object = event.as_object().unwrap();
        let expected = [
            "event.name",
            "event.outcome",
            "error.code",
            "duration_ms",
            "attempt",
            "principal.pseudonym",
            "principal.kind",
            "lab",
            "module",
            "feature.id",
            "release",
            "trace_id",
            "span_id",
            "agent.run_id",
            "items.returned",
            "items.refused",
        ];
        assert_eq!(
            object
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>(),
            expected.into_iter().collect()
        );
        for key in ["duration_ms", "attempt", "items.returned", "items.refused"] {
            assert!(event[key].is_u64());
        }
        for key in expected.into_iter().filter(|key| {
            !["duration_ms", "attempt", "items.returned", "items.refused"].contains(key)
        }) {
            assert!(event[key].is_string(), "{key}");
        }
        for feature in Feature::ALL {
            assert!(
                value(&telemetry, feature, "lab_one")["principal.pseudonym"].is_string(),
                "{} omitted the pseudonym",
                feature.id()
            );
        }
    }

    #[test]
    fn event_producers_are_refused_without_a_key() {
        for bind in [None, Some(OsString::from("127.0.0.1:32111"))] {
            let error = install_configuration(bind, None).unwrap_err();
            assert!(error.to_string().contains("requires a pseudonym key file"));
        }
    }

    #[test]
    fn pseudonyms_are_keyed_and_stable() {
        let keyed = events(vec![3; 32]);
        let first = value(&keyed, Feature::Package, "lab");
        let second = value(&keyed, Feature::Package, "lab");
        assert_eq!(first["principal.pseudonym"], second["principal.pseudonym"]);
        assert_ne!(first["principal.pseudonym"], Uuid::from_u128(1).to_string());
    }

    #[test]
    fn sentinel_free_text_cannot_reach_any_event_or_metric_label() {
        let telemetry = events(vec![9; 32]);
        let sentinel = "PRIVATE query prompt response@example.test /secret/file.csv 192.0.2.1";
        for feature in Feature::ALL {
            let event = value(&telemetry, feature, sentinel);
            assert!(!event.to_string().contains(sentinel));
        }
        assert!(!telemetry.metrics.render().contains(sentinel));
    }

    #[test]
    fn traceparent_accepts_only_bounded_w3c_identifiers() {
        let trace = TraceContext::from_traceparent(Some(
            "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01",
        ));
        assert_eq!(trace.trace_id, "0123456789abcdef0123456789abcdef");
        assert_eq!(trace.span_id, "0123456789abcdef");
        assert_ne!(
            TraceContext::from_traceparent(Some("private")).trace_id,
            "private"
        );
    }
}
