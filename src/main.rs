use std::{collections::HashMap, time::Duration, convert::Infallible};

use futures_util::{Stream, StreamExt};

use hyper::{service::{make_service_fn, service_fn}, Server, Response, Body, Request};
use once_cell::sync::{OnceCell, Lazy};

use opentelemetry::{sdk, metrics::{self, Counter, ValueRecorder, Unit}, global, KeyValue};

use opentelemetry_otlp::WithExportConfig;
use rand::Rng;

static PUSH_CONTROLLER: OnceCell<sdk::metrics::PushController> = OnceCell::new();
static METER: Lazy<metrics::Meter> = Lazy::new(|| global::meter("therock.metrics"));

#[derive(Debug)]
pub struct OtlpConfig {
    pub url: String,
    pub attributes: Option<HashMap<String, String>>,
    pub service: ServiceConfig,
}

#[derive(Debug, Default)]
pub struct ServiceConfig {
    pub max_event_per_span: Option<u32>,
    pub max_attributes_per_span: Option<u32>,
    pub max_links_per_span: Option<u32>,
    pub max_attributes_per_event: Option<u32>,
    pub max_attributes_per_link: Option<u32>,
    pub aggregator_selector: Option<AggregatorSelector>,
    pub export_kind: Option<ExportKind>,
}

#[derive(Debug)]
pub enum AggregatorSelector {
    Exact,
    Histogram { buckets: Vec<f64> },
    Inexpensive,
    Sketch { alpha: f64, max_num_bins: i64, key_epsilon: f64 },
}

impl Default for &AggregatorSelector {
    fn default() -> Self {
        &AggregatorSelector::Exact
    }
}

impl From<&AggregatorSelector> for sdk::metrics::selectors::simple::Selector {
    fn from(a: &AggregatorSelector) -> sdk::metrics::selectors::simple::Selector {
        match a {
            AggregatorSelector::Exact => sdk::metrics::selectors::simple::Selector::Exact,
            AggregatorSelector::Histogram { buckets } => {
                sdk::metrics::selectors::simple::Selector::Histogram(buckets.clone())
            }
            AggregatorSelector::Inexpensive => sdk::metrics::selectors::simple::Selector::Inexpensive,
            AggregatorSelector::Sketch { alpha, max_num_bins, key_epsilon } => {
                sdk::metrics::selectors::simple::Selector::Sketch(sdk::metrics::aggregators::DdSketchConfig::new(
                    *alpha,
                    *max_num_bins,
                    *key_epsilon,
                ))
            }
        }
    }
}

#[derive(Debug)]
pub enum ExportKind {
    Cumulative,
    Delta,
    Stateless,
}

impl Default for &ExportKind {
    fn default() -> Self {
        &ExportKind::Stateless
    }
}

impl From<&ExportKind> for sdk::export::metrics::ExportKindSelector {
    fn from(e: &ExportKind) -> Self {
        match e {
            ExportKind::Cumulative => sdk::export::metrics::ExportKindSelector::Cumulative,
            ExportKind::Delta => sdk::export::metrics::ExportKindSelector::Delta,
            ExportKind::Stateless => sdk::export::metrics::ExportKindSelector::Stateless,
        }
    }
}

fn get_metadata(attributes: Option<&HashMap<String, String>>) -> tonic::metadata::MetadataMap {
    let mut map = tonic::metadata::MetadataMap::with_capacity(attributes.map(|hm| hm.len()).unwrap_or_default());
    if let Some(a) = attributes {
        for (key, value) in a {
            map.entry(key).unwrap().or_insert_with(|| value.parse().unwrap());
        }
    }
    map
}

fn get_resources() -> [KeyValue; 4] {
    [
        KeyValue::new("service.name", "test-otlp"),
        KeyValue::new("service.namespace", "test"),
        KeyValue::new("service.instance.id", "development"),
        KeyValue::new("service.version", "0.1.0"),
    ]
}

fn delayed_interval(duration: Duration) -> impl Stream<Item = tokio::time::Instant> {
    opentelemetry::util::tokio_interval_stream(duration).skip(1)
}

fn start_metric_otlp(url: &str, attributes: Option<&HashMap<String, String>>, service: &ServiceConfig) {
    let push_controller = opentelemetry_otlp::new_pipeline()
        .metrics(tokio::spawn, delayed_interval)
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(url)
                .with_protocol(opentelemetry_otlp::Protocol::Grpc)
                .with_metadata(get_metadata(attributes)),
        )
        .with_aggregator_selector(sdk::metrics::selectors::simple::Selector::from(
            service.aggregator_selector.as_ref().unwrap_or_default(),
        ))
        .with_export_kind(sdk::export::metrics::ExportKindSelector::from(
            service.export_kind.as_ref().unwrap_or_default(),
        ))
        .with_resource(get_resources())
        .build()
        .unwrap();
    PUSH_CONTROLLER.set(push_controller).unwrap();
}

pub fn get_meter() -> &'static metrics::Meter {
    &METER
}

static HTTP_COUNTER: Lazy<Counter<u64>> = Lazy::new(|| {
    get_meter()
        .u64_counter("http.hits")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static HTTP_REQ_HISTOGRAM: Lazy<ValueRecorder<f64>> = Lazy::new(|| {
    get_meter()
        .f64_value_recorder("service.duration")
        .with_description("Service request latencies")
        .with_unit(Unit::new("s"))
        .init()
});

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = OtlpConfig {
        url: String::from("http://127.0.0.1:4317/"),
        attributes: None,
        service: ServiceConfig {
            aggregator_selector: Some(AggregatorSelector::Exact),
            export_kind: Some(ExportKind::Delta),
            ..Default::default()
        }
    };
    start_metric_otlp(&config.url, config.attributes.as_ref(), &config.service);

    // from now on, a simple hyper example

    let make_svc = make_service_fn(|_conn| {
        async { Ok::<_, Infallible>(service_fn(hello)) }
    });

    let addr = ([127, 0, 0, 1], 3000).into();

    let server = Server::bind(&addr).serve(make_svc);

    println!("Listening on http://{}", addr);

    server.await?;

    Ok(())
}

async fn hello(req: Request<Body>) -> Result<Response<Body>, Infallible> {
    let attributes = &[
        KeyValue::new("method", req.method().to_string()),
        KeyValue::new("path", req.uri().path().to_owned()),
        KeyValue::new("status", 200),
        KeyValue::new("failed", false),
    ];

    HTTP_COUNTER.add(1, attributes);
    HTTP_REQ_HISTOGRAM.record(rand::thread_rng().gen(), attributes);

    Ok(Response::new(Body::from("Hello World!")))
}
