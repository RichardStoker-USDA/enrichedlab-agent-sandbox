//! OpenTelemetry for the evidence append service. A tower layer gives every call one server span and the RPC metrics
//! (`rpc.server.requests`, `rpc.server.duration` in milliseconds), keyed by service, method and gRPC status code. It
//! sits outside the service's limits, so calls refused at the limit or past their deadline are counted with their
//! codes. The service also counts connections and token reviews. Everything is exported over OTLP/gRPC when the standard
//! `OTEL_EXPORTER_OTLP_ENDPOINT` is set and costs nothing otherwise. Attributes never carry request content, tokens or
//! producer identities.
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Instant,
};

use anyhow::{Context as _, Result};
use opentelemetry::{
    Context as OtelContext, KeyValue, global,
    metrics::{Counter, Histogram},
    propagation::{Extractor, TextMapPropagator},
    trace::{SpanKind, Status, TraceContextExt, Tracer},
};
use opentelemetry_sdk::{
    Resource, metrics::SdkMeterProvider, propagation::TraceContextPropagator,
    trace::SdkTracerProvider,
};
use tonic::codegen::http;
use tower::{Layer, Service};

const SCOPE: &str = "enrichedlab.evidence";
const KNOWN_METHODS: [(&str, &str, &str); 2] = [
    (
        "/enrichedlab.evidence.v1.EvidenceAppendService/Append",
        "enrichedlab.evidence.v1.EvidenceAppendService",
        "Append",
    ),
    (
        "/grpc.health.v1.Health/Check",
        "grpc.health.v1.Health",
        "Check",
    ),
];

/// The installed exporters, flushed and stopped on shutdown.
pub struct Providers {
    tracer: Option<SdkTracerProvider>,
    meter: Option<SdkMeterProvider>,
}

impl Providers {
    pub fn shutdown(self) {
        if let Some(tracer) = self.tracer {
            let _ = tracer.shutdown();
        }
        if let Some(meter) = self.meter {
            let _ = meter.shutdown();
        }
    }
}

/// Installs OTLP trace and metric export when an endpoint is configured.
pub fn install(service_name: &str) -> Result<Providers> {
    if std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_none() {
        return Ok(Providers {
            tracer: None,
            meter: None,
        });
    }
    let resource = Resource::builder()
        .with_service_name(service_name.to_owned())
        .build();
    let spans = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .build()
        .context("failed to build the OTLP span exporter")?;
    let tracer = SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(spans)
        .build();
    global::set_tracer_provider(tracer.clone());
    let metrics = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .build()
        .context("failed to build the OTLP metric exporter")?;
    let meter = SdkMeterProvider::builder()
        .with_resource(resource)
        .with_periodic_exporter(metrics)
        .build();
    global::set_meter_provider(meter.clone());
    Ok(Providers {
        tracer: Some(tracer),
        meter: Some(meter),
    })
}

/// The service's metric names. A site's OpenTelemetry gateway may keep only metric names under known prefixes
/// (`enrichedlab.` and the semantic-convention families such as `rpc.`) and drop the rest without a trace, so the
/// service's own names sit under `enrichedlab.evidence.` and the call metrics follow the RPC semantic conventions (tested
/// below).
pub const RPC_REQUESTS: &str = "rpc.server.requests";
pub const RPC_DURATION: &str = "rpc.server.duration";
pub const TOKEN_REVIEWS: &str = "enrichedlab.evidence.token_reviews";
pub const CONNECTIONS: &str = "enrichedlab.evidence.connections";
pub const CAPTURE_LOG_OLDEST: &str = "enrichedlab.evidence.capture.log_oldest_seconds";
pub const CAPTURE_STALLED_RUNS: &str = "enrichedlab.evidence.capture.recorder_stalled_runs";
#[cfg(test)]
const OWN_METRIC_NAMES: [&str; 4] = [
    TOKEN_REVIEWS,
    CONNECTIONS,
    CAPTURE_LOG_OLDEST,
    CAPTURE_STALLED_RUNS,
];
#[cfg(test)]
const RPC_METRIC_NAMES: [&str; 2] = [RPC_REQUESTS, RPC_DURATION];

/// A counter in the service's scope, for events outside a call (connections, token reviews).
pub fn counter(name: &'static str, description: &'static str) -> Counter<u64> {
    global::meter(SCOPE)
        .u64_counter(name)
        .with_description(description)
        .build()
}

/// A gauge in the service's scope, for a level read on a timer (the capture log's age).
pub fn gauge(name: &'static str, description: &'static str) -> opentelemetry::metrics::Gauge<u64> {
    global::meter(SCOPE)
        .u64_gauge(name)
        .with_description(description)
        .build()
}

#[derive(Clone)]
pub struct RpcTelemetryLayer {
    requests: Counter<u64>,
    duration: Histogram<f64>,
}

impl RpcTelemetryLayer {
    /// Instruments from the installed meter provider.
    pub fn new() -> Self {
        Self::with_meter(&global::meter(SCOPE))
    }

    /// Instruments from a given meter (tests record through their own provider).
    pub fn with_meter(meter: &opentelemetry::metrics::Meter) -> Self {
        Self {
            requests: meter
                .u64_counter(RPC_REQUESTS)
                .with_description("Calls by service, method and gRPC status code")
                .build(),
            duration: meter
                .f64_histogram(RPC_DURATION)
                .with_unit("ms")
                .with_description("Call duration by service, method and gRPC status code")
                .build(),
        }
    }
}

impl Default for RpcTelemetryLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for RpcTelemetryLayer {
    type Service = RpcTelemetry<S>;

    fn layer(&self, inner: S) -> Self::Service {
        RpcTelemetry {
            inner,
            requests: self.requests.clone(),
            duration: self.duration.clone(),
        }
    }
}

#[derive(Clone)]
pub struct RpcTelemetry<S> {
    inner: S,
    requests: Counter<u64>,
    duration: Histogram<f64>,
}

struct Headers<'a>(&'a http::HeaderMap);

impl Extractor for Headers<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(http::HeaderName::as_str).collect()
    }
}

/// The call's service and method when it is one this service serves; anything else is reported as `other`, so a
/// caller cannot grow the metric's label set.
fn method_of(path: &str) -> (&'static str, &'static str) {
    KNOWN_METHODS
        .iter()
        .find(|(known, _, _)| *known == path)
        .map(|(_, service, method)| (*service, *method))
        .unwrap_or(("other", "other"))
}

/// The response's gRPC status code: errors answer with it in the headers (trailers-only); success carries 0 in the
/// trailers, which a unary call's headers do not show.
fn status_code<B>(result: &Result<http::Response<B>, impl Sized>) -> i64 {
    match result {
        Ok(response) => response
            .headers()
            .get("grpc-status")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        Err(_) => 2,
    }
}

impl<S, B, R> Service<http::Request<B>> for RpcTelemetry<S>
where
    S: Service<http::Request<B>, Response = http::Response<R>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Send + 'static,
    B: Send + 'static,
    R: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<B>) -> Self::Future {
        let (service, method) = method_of(request.uri().path());
        let parent = TraceContextPropagator::new().extract(&Headers(request.headers()));
        let tracer = global::tracer(SCOPE);
        let span = tracer
            .span_builder(format!("{service}/{method}"))
            .with_kind(SpanKind::Server)
            .with_attributes([
                KeyValue::new("rpc.system", "grpc"),
                KeyValue::new("rpc.service", service),
                KeyValue::new("rpc.method", method),
            ])
            .start_with_context(&tracer, &parent);
        let mut call = Call {
            context: OtelContext::current_with_span(span),
            started: Instant::now(),
            requests: self.requests.clone(),
            duration: self.duration.clone(),
            service,
            method,
            done: false,
        };
        let future = self.inner.call(request);
        Box::pin(async move {
            let result = future.await;
            call.finish(status_code(&result));
            result
        })
    }
}

/// One call's span and measurements. A call dropped before it answers (the client went away, or an
/// outer deadline cut it short) is recorded as CANCELLED when it is dropped, so no call goes uncounted.
struct Call {
    context: OtelContext,
    started: Instant,
    requests: Counter<u64>,
    duration: Histogram<f64>,
    service: &'static str,
    method: &'static str,
    done: bool,
}

impl Call {
    fn finish(&mut self, code: i64) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        let attributes = [
            KeyValue::new("rpc.system", "grpc"),
            KeyValue::new("rpc.service", self.service),
            KeyValue::new("rpc.method", self.method),
            KeyValue::new("rpc.grpc.status_code", code),
        ];
        self.requests.add(1, &attributes);
        self.duration
            .record(self.started.elapsed().as_secs_f64() * 1000.0, &attributes);
        let span = self.context.span();
        span.set_attribute(KeyValue::new("rpc.grpc.status_code", code));
        if code != 0 {
            span.set_status(Status::error("grpc error"));
        }
        span.end();
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        self.finish(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        OWN_METRIC_NAMES, RPC_METRIC_NAMES, RPC_REQUESTS, RpcTelemetryLayer, method_of, status_code,
    };

    #[test]
    fn every_metric_name_passes_the_gateway_prefix_rule() {
        // The prefixes a gateway's metric-name filter keeps for this service: its own and the RPC conventions.
        const KEPT: [&str; 2] = ["enrichedlab.", "rpc."];
        for name in OWN_METRIC_NAMES.into_iter().chain(RPC_METRIC_NAMES) {
            assert!(KEPT.iter().any(|prefix| name.starts_with(prefix)), "{name}");
            assert!(
                name.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "{name}"
            );
        }
        for name in OWN_METRIC_NAMES {
            assert!(name.starts_with("enrichedlab.evidence."), "{name}");
        }
        for name in RPC_METRIC_NAMES {
            assert!(name.starts_with("rpc.server."), "{name}");
        }
    }
    use tonic::codegen::http;
    use tower::{Layer, Service};

    #[tokio::test]
    async fn a_call_dropped_before_it_answers_is_counted_as_cancelled() {
        let exporter = opentelemetry_sdk::metrics::InMemoryMetricExporter::default();
        let provider = opentelemetry_sdk::metrics::SdkMeterProvider::builder()
            .with_periodic_exporter(exporter.clone())
            .build();
        let meter = opentelemetry::metrics::MeterProvider::meter(&provider, "test");
        let never = tower::service_fn(|_: http::Request<()>| {
            std::future::pending::<Result<http::Response<()>, std::convert::Infallible>>()
        });
        let mut service = RpcTelemetryLayer::with_meter(&meter).layer(never);
        let request = http::Request::builder()
            .uri("/enrichedlab.evidence.v1.EvidenceAppendService/Append")
            .body(())
            .unwrap();
        let call = service.call(request);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), call)
                .await
                .is_err()
        );
        provider.force_flush().unwrap();
        let mut codes = Vec::new();
        for resource in exporter.get_finished_metrics().unwrap() {
            for scope in resource.scope_metrics() {
                for metric in scope
                    .metrics()
                    .filter(|metric| metric.name() == RPC_REQUESTS)
                {
                    if let opentelemetry_sdk::metrics::data::AggregatedMetrics::U64(
                        opentelemetry_sdk::metrics::data::MetricData::Sum(sum),
                    ) = metric.data()
                    {
                        for point in sum.data_points() {
                            for attribute in point.attributes() {
                                if attribute.key.as_str() == "rpc.grpc.status_code" {
                                    codes.push((attribute.value.to_string(), point.value()));
                                }
                            }
                        }
                    }
                }
            }
        }
        assert!(codes.contains(&("1".to_string(), 1)), "{codes:?}");
    }

    #[test]
    fn unknown_paths_do_not_become_labels() {
        assert_eq!(
            method_of("/enrichedlab.evidence.v1.EvidenceAppendService/Append"),
            ("enrichedlab.evidence.v1.EvidenceAppendService", "Append")
        );
        assert_eq!(method_of("/anything/else"), ("other", "other"));
    }

    #[test]
    fn status_comes_from_trailers_only_headers() {
        let refused = http::Response::builder()
            .header("grpc-status", "7")
            .body(())
            .unwrap();
        assert_eq!(status_code::<()>(&Ok::<_, ()>(refused)), 7);
        assert_eq!(status_code::<()>(&Ok::<_, ()>(http::Response::new(()))), 0);
        assert_eq!(status_code::<()>(&Err::<http::Response<()>, _>(())), 2);
    }
}
