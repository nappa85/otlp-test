use std::{collections::HashMap, convert::Infallible, env, sync::Arc};

use hyper::{
    service::{make_service_fn, service_fn},
    Body, Request, Response, Server,
};
use once_cell::sync::{Lazy, OnceCell};

use opentelemetry::{
    global,
    metrics::{
        self, Counter, Histogram, ObservableCounter, ObservableGauge, ObservableUpDownCounter,
        Unit, UpDownCounter,
    },
    sdk::{
        self,
        metrics::{aggregators::Aggregator, sdk_api::Descriptor},
    },
    Context, KeyValue,
};

use opentelemetry_otlp::WithExportConfig;
use rand::Rng;

static PUSH_CONTROLLER: OnceCell<sdk::metrics::controllers::BasicController> = OnceCell::new();
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
    // pub aggregator_selector: Option<AggregatorSelector>,
    pub export_kind: ExportKind,
}

#[derive(Copy, Clone, Debug)]
pub enum ExportKind {
    Cumulative,
    Delta,
    Stateless,
}

impl ExportKind {
    fn from_env() -> Option<Self> {
        match env::var("EXPORT_KIND").as_deref() {
            Ok("CUMULATIVE") => Some(ExportKind::Cumulative),
            Ok("DELTA") => Some(ExportKind::Delta),
            Ok("STATELESS") => Some(ExportKind::Stateless),
            x => {
                eprintln!("EXPORT_KIND {x:?}");
                None
            }
        }
    }
}

impl Default for ExportKind {
    fn default() -> Self {
        ExportKind::Stateless
    }
}

impl sdk::export::metrics::aggregation::TemporalitySelector for ExportKind {
    fn temporality_for(
        &self,
        descriptor: &sdk::metrics::sdk_api::Descriptor,
        kind: &sdk::export::metrics::aggregation::AggregationKind,
    ) -> sdk::export::metrics::aggregation::Temporality {
        match self {
            ExportKind::Cumulative => {
                sdk::export::metrics::aggregation::cumulative_temporality_selector()
                    .temporality_for(descriptor, kind)
            }
            ExportKind::Delta => sdk::export::metrics::aggregation::delta_temporality_selector()
                .temporality_for(descriptor, kind),
            ExportKind::Stateless => {
                sdk::export::metrics::aggregation::stateless_temporality_selector()
                    .temporality_for(descriptor, kind)
            }
        }
    }
}

fn get_metadata(attributes: Option<&HashMap<String, String>>) -> tonic::metadata::MetadataMap {
    let mut map = tonic::metadata::MetadataMap::with_capacity(
        attributes.map(|hm| hm.len()).unwrap_or_default(),
    );
    if let Some(a) = attributes {
        for (key, value) in a {
            map.entry(key)
                .unwrap()
                .or_insert_with(|| value.parse().unwrap());
        }
    }
    map
}

pub fn get_resources() -> sdk::Resource {
    sdk::Resource::new([
        KeyValue::new("service.name", "test-otlp"),
        KeyValue::new("service.namespace", "test"),
        KeyValue::new("service.instance.id", "development"),
        KeyValue::new("service.version", "0.1.0"),
    ])
}

#[derive(Debug, Default)]
struct AggregatorSelector;

impl sdk::export::metrics::AggregatorSelector for AggregatorSelector {
    fn aggregator_for(&self, descriptor: &Descriptor) -> Option<Arc<dyn Aggregator + Send + Sync>> {
        match descriptor.name() {
            // name if name.ends_with(".disabled") => None,
            name if name.ends_with(".sum") => Some(Arc::new(sdk::metrics::aggregators::sum())),
            name if name.ends_with(".last") => {
                Some(Arc::new(sdk::metrics::aggregators::last_value()))
            }
            name if name.ends_with(".histogram") => {
                let boundaries = (0..100)
                    .into_iter()
                    .map(|i| i as f64 / 10.0)
                    .collect::<Vec<_>>();
                Some(Arc::new(sdk::metrics::aggregators::histogram(&boundaries)))
            }
            _ => panic!(
                "Invalid instrument name for test AggregatorSelector: {}",
                descriptor.name()
            ),
        }
    }
}

fn start_metric_otlp(
    url: &str,
    attributes: Option<&HashMap<String, String>>,
    service: &ServiceConfig,
) {
    let controller = opentelemetry_otlp::new_pipeline()
        .metrics(
            AggregatorSelector,
            service.export_kind,
            opentelemetry::runtime::Tokio,
        )
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(url)
                .with_protocol(opentelemetry_otlp::Protocol::Grpc)
                .with_metadata(get_metadata(attributes)),
        )
        .with_resource(get_resources())
        .build()
        .unwrap();
    PUSH_CONTROLLER.set(controller).unwrap();
}

pub fn get_meter() -> &'static metrics::Meter {
    &METER
}

static U64_COUNTER_SUM: Lazy<Counter<u64>> = Lazy::new(|| {
    get_meter()
        .u64_counter("u64.counter.sum")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static U64_COUNTER_LAST: Lazy<Counter<u64>> = Lazy::new(|| {
    get_meter()
        .u64_counter("u64.counter.last")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static U64_COUNTER_HISTOGRAM: Lazy<Counter<u64>> = Lazy::new(|| {
    get_meter()
        .u64_counter("u64.counter.histogram")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});

static U64_HISTOGRAM_SUM: Lazy<Histogram<u64>> = Lazy::new(|| {
    get_meter()
        .u64_histogram("u64.histogram.sum")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static U64_HISTOGRAM_LAST: Lazy<Histogram<u64>> = Lazy::new(|| {
    get_meter()
        .u64_histogram("u64.histogram.last")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static U64_HISTOGRAM_HISTOGRAM: Lazy<Histogram<u64>> = Lazy::new(|| {
    get_meter()
        .u64_histogram("u64.histogram.histogram")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});

static U64_OBSERVABLE_COUNTER_SUM: Lazy<ObservableCounter<u64>> = Lazy::new(|| {
    get_meter()
        .u64_observable_counter("u64.observable_counter.sum")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static U64_OBSERVABLE_COUNTER_LAST: Lazy<ObservableCounter<u64>> = Lazy::new(|| {
    get_meter()
        .u64_observable_counter("u64.observable_counter.last")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static U64_OBSERVABLE_COUNTER_HISTOGRAM: Lazy<ObservableCounter<u64>> = Lazy::new(|| {
    get_meter()
        .u64_observable_counter("u64.observable_counter.histogram")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});

static U64_OBSERVABLE_GAUGE_SUM: Lazy<ObservableGauge<u64>> = Lazy::new(|| {
    get_meter()
        .u64_observable_gauge("u64.observable_gauge.sum")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static U64_OBSERVABLE_GAUGE_LAST: Lazy<ObservableGauge<u64>> = Lazy::new(|| {
    get_meter()
        .u64_observable_gauge("u64.observable_gauge.last")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static U64_OBSERVABLE_GAUGE_HISTOGRAM: Lazy<ObservableGauge<u64>> = Lazy::new(|| {
    get_meter()
        .u64_observable_gauge("u64.observable_gauge.histogram")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});

static F64_COUNTER_SUM: Lazy<Counter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_counter("f64.counter.sum")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_COUNTER_LAST: Lazy<Counter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_counter("f64.counter.last")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_COUNTER_HISTOGRAM: Lazy<Counter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_counter("f64.counter.histogram")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});

static F64_HISTOGRAM_SUM: Lazy<Histogram<f64>> = Lazy::new(|| {
    get_meter()
        .f64_histogram("f64.histogram.sum")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_HISTOGRAM_LAST: Lazy<Histogram<f64>> = Lazy::new(|| {
    get_meter()
        .f64_histogram("f64.histogram.last")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_HISTOGRAM_HISTOGRAM: Lazy<Histogram<f64>> = Lazy::new(|| {
    get_meter()
        .f64_histogram("f64.histogram.histogram")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});

static F64_OBSERVABLE_COUNTER_SUM: Lazy<ObservableCounter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_observable_counter("f64.observable_counter.sum")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_OBSERVABLE_COUNTER_LAST: Lazy<ObservableCounter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_observable_counter("f64.observable_counter.last")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_OBSERVABLE_COUNTER_HISTOGRAM: Lazy<ObservableCounter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_observable_counter("f64.observable_counter.histogram")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});

static F64_OBSERVABLE_GAUGE_SUM: Lazy<ObservableGauge<f64>> = Lazy::new(|| {
    get_meter()
        .f64_observable_gauge("f64.observable_gauge.sum")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_OBSERVABLE_GAUGE_LAST: Lazy<ObservableGauge<f64>> = Lazy::new(|| {
    get_meter()
        .f64_observable_gauge("f64.observable_gauge.last")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_OBSERVABLE_GAUGE_HISTOGRAM: Lazy<ObservableGauge<f64>> = Lazy::new(|| {
    get_meter()
        .f64_observable_gauge("f64.observable_gauge.histogram")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});

static F64_OBSERVABLE_UP_DOWN_COUNTER_SUM: Lazy<ObservableUpDownCounter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_observable_up_down_counter("f64.observable_up_down_counter.sum")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_OBSERVABLE_UP_DOWN_COUNTER_LAST: Lazy<ObservableUpDownCounter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_observable_up_down_counter("f64.observable_up_down_counter.last")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_OBSERVABLE_UP_DOWN_COUNTER_HISTOGRAM: Lazy<ObservableUpDownCounter<f64>> =
    Lazy::new(|| {
        get_meter()
            .f64_observable_up_down_counter("f64.observable_up_down_counter.histogram")
            .with_description("Request hit counter")
            .with_unit(Unit::new("r"))
            .init()
    });

static F64_UP_DOWN_COUNTER_SUM: Lazy<UpDownCounter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_up_down_counter("f64.up_down_counter.sum")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_UP_DOWN_COUNTER_LAST: Lazy<UpDownCounter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_up_down_counter("f64.up_down_counter.last")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});
static F64_UP_DOWN_COUNTER_HISTOGRAM: Lazy<UpDownCounter<f64>> = Lazy::new(|| {
    get_meter()
        .f64_up_down_counter("f64.up_down_counter.histogram")
        .with_description("Request hit counter")
        .with_unit(Unit::new("r"))
        .init()
});

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = OtlpConfig {
        url: String::from("http://127.0.0.1:4317/"),
        attributes: None,
        service: ServiceConfig {
            // aggregator_selector: AggregatorSelector::from_env(),
            export_kind: ExportKind::from_env().unwrap_or_default(),
            ..Default::default()
        },
    };
    start_metric_otlp(&config.url, config.attributes.as_ref(), &config.service);

    // from now on, a simple hyper example

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(hello)) });

    let addr = ([127, 0, 0, 1], 3000).into();

    let server = Server::bind(&addr).serve(make_svc);

    println!(
        "Version {} listening on http://{addr}",
        env!("CARGO_PKG_VERSION")
    );

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

    let cx = Context::current();

    U64_COUNTER_SUM.add(&cx, 1, attributes);
    U64_COUNTER_LAST.add(&cx, 1, attributes);
    U64_COUNTER_HISTOGRAM.add(&cx, 1, attributes);
    U64_HISTOGRAM_SUM.record(&cx, 1, attributes);
    U64_HISTOGRAM_LAST.record(&cx, 1, attributes);
    U64_HISTOGRAM_HISTOGRAM.record(&cx, 1, attributes);
    U64_OBSERVABLE_COUNTER_SUM.observe(&cx, 1, attributes);
    U64_OBSERVABLE_COUNTER_LAST.observe(&cx, 1, attributes);
    U64_OBSERVABLE_COUNTER_HISTOGRAM.observe(&cx, 1, attributes);
    U64_OBSERVABLE_GAUGE_SUM.observe(&cx, 1, attributes);
    U64_OBSERVABLE_GAUGE_LAST.observe(&cx, 1, attributes);
    U64_OBSERVABLE_GAUGE_HISTOGRAM.observe(&cx, 1, attributes);

    let val: f64 = rand::thread_rng().gen();
    F64_COUNTER_SUM.add(&cx, val, attributes);
    F64_COUNTER_LAST.add(&cx, val, attributes);
    F64_COUNTER_HISTOGRAM.add(&cx, val, attributes);
    F64_HISTOGRAM_SUM.record(&cx, val, attributes);
    F64_HISTOGRAM_LAST.record(&cx, val, attributes);
    F64_HISTOGRAM_HISTOGRAM.record(&cx, val, attributes);
    F64_OBSERVABLE_COUNTER_SUM.observe(&cx, val, attributes);
    F64_OBSERVABLE_COUNTER_LAST.observe(&cx, val, attributes);
    F64_OBSERVABLE_COUNTER_HISTOGRAM.observe(&cx, val, attributes);
    F64_OBSERVABLE_GAUGE_SUM.observe(&cx, val, attributes);
    F64_OBSERVABLE_GAUGE_LAST.observe(&cx, val, attributes);
    F64_OBSERVABLE_GAUGE_HISTOGRAM.observe(&cx, val, attributes);
    F64_OBSERVABLE_UP_DOWN_COUNTER_SUM.observe(&cx, val, attributes);
    F64_OBSERVABLE_UP_DOWN_COUNTER_LAST.observe(&cx, val, attributes);
    F64_OBSERVABLE_UP_DOWN_COUNTER_HISTOGRAM.observe(&cx, val, attributes);
    F64_UP_DOWN_COUNTER_SUM.add(&cx, val, attributes);
    F64_UP_DOWN_COUNTER_LAST.add(&cx, val, attributes);
    F64_UP_DOWN_COUNTER_HISTOGRAM.add(&cx, val, attributes);

    Ok(Response::new(Body::from("Hello World!")))
}
