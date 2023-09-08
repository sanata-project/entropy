use std::error::Error;

use actix_web::{
    get, post,
    web::{Bytes, Path},
    App, HttpServer,
};
use actix_web_opentelemetry::ClientExt;
use opentelemetry::{
    global,
    sdk::{trace, Resource},
    trace::FutureExt,
    Context, KeyValue,
};
use tokio::{sync::mpsc, task::spawn_local};
use tracing::instrument;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[post("/client-request/{path}")]
#[instrument]
async fn client_request(path: Path<String>) -> Bytes {
    let mut message = mpsc::unbounded_channel();
    message
        .0
        .send((path.into_inner(), Context::current()))
        .unwrap();
    let mut result = mpsc::unbounded_channel();
    spawn_local(async move {
        let (path, context) = message.1.recv().await.unwrap();
        let _attach = context.attach();
        spawn_local(
            async move {
                result
                    .0
                    .send(
                        awc::Client::new()
                            .get(format!("http://127.0.0.1:8080/{path}"))
                            .trace_request()
                            .send()
                            .await
                            .unwrap()
                            .body()
                            .await
                            .unwrap(),
                    )
                    .unwrap()
            }
            .with_current_context(),
        );
    });
    result.1.recv().await.unwrap()
}

#[get("/{path}")]
#[instrument]
async fn echo(path: Path<String>) -> String {
    tracing::info!(?path, "echo");
    format!("OK {path}")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    global::set_text_map_propagator(opentelemetry_jaeger::Propagator::new());
    let otlp_exporter = opentelemetry_otlp::new_exporter().tonic();
    let tracer = opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(otlp_exporter)
        .with_trace_config(
            trace::config().with_resource(Resource::new(vec![KeyValue::new(
                "service.name",
                "tracing_demo",
            )])),
        )
        .install_batch(opentelemetry::runtime::Tokio)
        .expect("unable to install OTLP tracer");

    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_opentelemetry::layer().with_tracer(tracer))
        .init();

    HttpServer::new(move || {
        App::new()
            .wrap(actix_web_opentelemetry::RequestTracing::new())
            .service(client_request)
            .service(echo)
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await?;

    global::shutdown_tracer_provider(); // sending remaining spans

    Ok(())
}
