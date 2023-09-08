use opentelemetry::sdk::{trace, Resource};
use opentelemetry::KeyValue;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

pub fn setup_tracing(pairs: impl IntoIterator<Item = KeyValue>) {
    opentelemetry::global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(opentelemetry_otlp::new_exporter().tonic())
        .with_trace_config(trace::config().with_resource(Resource::new(pairs)))
        .install_batch(opentelemetry::runtime::Tokio)
        .expect("unable to install OTLP tracer");
    tracing_subscriber::registry()
        // .with(tracing_subscriber::fmt::layer().json())
        .with(EnvFilter::from_default_env())
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .init();
}

pub async fn shutdown_tracing() {
    tokio::task::spawn_blocking(opentelemetry::global::shutdown_tracer_provider)
        .await
        .unwrap()
}

// i know there's `hex` and `itertools` in the wild, just want to avoid
// introduce util dependencies for single use case
pub fn hex_string(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .reduce(|s1, s2| s1 + &s2)
        .unwrap()
}
