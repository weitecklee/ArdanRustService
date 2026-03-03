use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Extension, Json, Router, async_trait};
use config::{AsyncSource, Config as Cfg, ConfigError, FileFormat, Format, Map};
use opentelemetry::{KeyValue, global, logs::LogError, trace::TraceError};
use opentelemetry_otlp::{ExportConfig, WithExportConfig};
use opentelemetry_sdk::trace as sdktrace; // To avoid name conflicts
use opentelemetry_sdk::{
    Resource, logs::Config, metrics::MeterProvider, propagation::TraceContextPropagator, runtime,
};
use serde::{Deserialize, Serialize};
use tower::ServiceBuilder;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing::{Level, error, info, instrument, level_filters::LevelFilter};
// use tracing_subscriber::fmt::format::FmtSpan;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa::{OpenApi, ToSchema};
use utoipa_redoc::{Redoc, Servable};
use utoipa_swagger_ui::SwaggerUi;

struct MyCounter {
    counter: AtomicUsize,
}

struct MyConfig {
    text: String,
}

struct MyState(i32);

struct Counter {
    count: AtomicUsize,
}

#[derive(Clone)]
struct AuthHeader {
    id: String,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct EnvConfig {
    test_toml: String,
    testvar: String,
    test_setting: String,
}

#[derive(Debug)]
struct HttpSource<F: Format> {
    uri: String,
    format: F,
}

#[async_trait]
impl<F: Format + Send + Sync + Debug> AsyncSource for HttpSource<F> {
    async fn collect(&self) -> Result<Map<String, config::Value>, ConfigError> {
        reqwest::get(&self.uri)
            .await
            .map_err(|e| ConfigError::Foreign(Box::new(e)))? // error conversion is possible from custom AsyncSource impls
            .text()
            .await
            .map_err(|e| ConfigError::Foreign(Box::new(e)))
            .and_then(|text| {
                self.format
                    .parse(Some(&self.uri), &text)
                    .map_err(ConfigError::Foreign)
            })
    }
}

#[tokio::main]
async fn main() {
    tokio::spawn(settings_server());
    tokio::time::sleep(Duration::from_secs(1)).await;
    let _ = dotenvy::dotenv();

    let settings_reader = Cfg::builder()
        .add_source(config::File::with_name("settings").required(false))
        .add_source(config::Environment::with_prefix("APP"))
        .add_async_source(HttpSource {
            uri: "http://localhost:3002/".into(),
            format: FileFormat::Toml,
        })
        .build()
        .await
        .unwrap();

    let settings = settings_reader.try_deserialize::<EnvConfig>().unwrap();

    println!("{settings:#?}");

    #[derive(OpenApi)]
    #[openapi(
        paths(
            handler,
            service_one,
            service_two,
            counter_sv,
            query_extract,
            path_extract,
            header_handler,
            status_handler,
        ),
        components(
            schemas(VisitorNumber),
        ),
        modifiers(),
        tags(
            (name = "Test System", description = "A really simple API")
        )
    )]
    struct ApiDoc;

    global::set_text_map_propagator(TraceContextPropagator::new());

    let otlp_endpoint = "http://localhost:4317";

    let tracer = init_tracer(otlp_endpoint).unwrap();

    let telemetry_layer = tracing_opentelemetry::layer().with_tracer(tracer);
    let subscriber = tracing_subscriber::registry()
        .with(LevelFilter::from_level(Level::DEBUG))
        .with(telemetry_layer);

    subscriber.init();

    let _meter_provider = init_metrics(otlp_endpoint);
    let _log_provider = init_logs(otlp_endpoint);

    // // setup tracing
    // let file_appender = tracing_appender::rolling::hourly("test.log", "prefix.log");
    // let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    // let subscriber = tracing_subscriber::fmt()
    //     .compact()
    //     .with_file(true)
    //     .with_line_number(true)
    //     .with_thread_ids(true)
    //     .with_target(false)
    //     .with_span_events(FmtSpan::CLOSE)
    //     .with_writer(non_blocking)
    //     .json()
    //     .finish();
    // // setup subscriber as default
    // tracing::subscriber::set_global_default(subscriber).unwrap();

    let testvar = std::env::var("TESTVAR").unwrap_or_else(|_| "default".to_string());
    info!("Starting server");
    info!("testvar = {}", testvar);

    let shared_counter = Arc::new(MyCounter {
        counter: AtomicUsize::new(0),
    });

    let shared_text = Arc::new(MyConfig {
        text: "This is my configuration.".to_string(),
    });

    let other = Router::new().route("/other", get(async || Html("The other route")));

    let service = ServiceBuilder::new()
        .layer(CompressionLayer::new())
        .layer(
            CorsLayer::new()
                .allow_methods([Method::GET, Method::POST])
                .allow_origin(Any),
        )
        .layer(ConcurrencyLimitLayer::new(100));

    let app = Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .merge(Redoc::with_url("/redoc", ApiDoc::openapi()))
        .nest("/1", service_one())
        .nest("/2", service_two())
        .nest("/counter", counter_sv())
        .route("/", get(handler))
        .route("/book/{id}", get(path_extract))
        .route("/book", get(query_extract))
        .route("/header", get(header_handler))
        .route("/status", get(status_handler))
        .layer(Extension(shared_counter))
        .layer(Extension(shared_text))
        .fallback_service(ServeDir::new("web"))
        // .route_layer(axum::middleware::from_fn(auth))
        .merge(other)
        .route("/warandpeace", get(war_and_peace_handler))
        .layer(service.into_inner())
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request<Body>| {
                let request_id = uuid::Uuid::new_v4();
                tracing::span!(
                    tracing::Level::INFO,
                    "request",
                    method = tracing::field::display(request.method()),
                    uri = tracing::field::display(request.uri()),
                    version = tracing::field::debug(request.version()),
                    request_id = tracing::field::display(request_id),
                )
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3001")
        .await
        .unwrap();

    tokio::spawn(make_request());

    info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

async fn settings_server() {
    let app = Router::new().route("/", get(|| async { "test_setting = \"fromhttp\"" }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3002")
        .await
        .unwrap();

    println!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

#[utoipa::path(
    get,
    path = "/1",
    responses(
        (status = 200, description = "Access service 1", body = [String])
    )
)]
fn service_one() -> Router {
    let state = Arc::new(MyState(5));
    Router::new().route("/", get(sv1_handler)).with_state(state)
}

async fn sv1_handler(
    Extension(counter): Extension<Arc<MyCounter>>,
    State(state): State<Arc<MyState>>,
) -> Html<String> {
    counter
        .counter
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Html(format!(
        "Service {}-{}",
        counter.counter.load(std::sync::atomic::Ordering::Relaxed),
        state.0,
    ))
}

#[utoipa::path(
    get,
    path = "/2",
    responses(
        (status = 200, description = "Access service 2", body = [String])
    )
)]
fn service_two() -> Router {
    Router::new().route("/", get(|| async { Html("Service Two".to_string()) }))
}

#[utoipa::path(
    get,
    path = "/counter",
    responses(
        (status = 200, description = "Access counter service", body = [String])
    )
)]
fn counter_sv() -> Router {
    let counter = Arc::new(Counter {
        count: AtomicUsize::new(0),
    });

    Router::new()
        .route("/", get(counter_handler))
        .route("/inc", get(counter_inc))
        .with_state(counter)
}

async fn counter_handler() -> Html<String> {
    println!("Sending GET request");
    let current_count = reqwest::get("http://localhost:3001/counter/inc")
        .await
        .unwrap()
        .json::<i32>()
        .await
        .unwrap();
    Html(format!("<h1>Remote Counter: {current_count} </h1>"))
}

async fn counter_inc(State(counter): State<Arc<Counter>>) -> Json<usize> {
    println!("/inc service called");
    let current_value = counter
        .count
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Json(current_value)
}

#[derive(Serialize, ToSchema)]
struct VisitorNumber {
    message: String,
}

#[utoipa::path(
    get,
    path = "/",
    responses(
        (status = 200, description = "Display visitor number", body = [VisitorNumber])
    )
)]
async fn handler(
    Extension(counter): Extension<Arc<MyCounter>>,
    Extension(config): Extension<Arc<MyConfig>>,
) -> Html<String> {
    counter
        .counter
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Html(format!(
        "<h1>{} You are visitor number {}!</h1>",
        config.text,
        counter.counter.load(std::sync::atomic::Ordering::Relaxed)
    ))
}

#[utoipa::path(
    get,
    path = "/book/{id}",
    responses(
        (status = 200, description = "Extract id from path", body = [String])
    )
)]
async fn path_extract(Path(id): Path<u32>) -> Html<String> {
    Html(format!("Hello, {id}!"))
}

#[utoipa::path(
    get,
    path = "/book",
    responses(
        (status = 200, description = "Extract id from parameters", body = [String])
    )
)]
async fn query_extract(Query(params): Query<HashMap<String, String>>) -> Html<String> {
    Html(format!("{params:#?}"))
}

#[utoipa::path(
    get,
    path = "/header",
    responses(
        (status = 200, description = "Extract id from header", body = [String])
    )
)]
async fn header_handler(Extension(auth): Extension<AuthHeader>) -> Html<String> {
    Html(format!("x-request-id: {}", auth.id))
}

#[utoipa::path(
    get,
    path = "/status",
    responses(
        (status = 200, description = "Display status", body = [String]),
        (status = StatusCode::INTERNAL_SERVER_ERROR, description = "internal server error", body = [String])
    )
)]
async fn status_handler() -> Result<impl IntoResponse, (StatusCode, String)> {
    let start = std::time::SystemTime::now();
    let seconds_wrapped = start
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "Bad clock".to_string()))?
        .as_secs()
        % 3;
    let divided = 100u64
        .checked_div(seconds_wrapped)
        .ok_or((StatusCode::INTERNAL_SERVER_ERROR, "div by 0".to_string()))?;

    Ok(Json(divided))
}

async fn make_request() {
    // Pause to let the server start up
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Make a request to the server
    let _response = reqwest::Client::new()
        .get("http://localhost:3001/header")
        .header("x-request-id", "1234")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    // info!("{}", response);

    let _response = reqwest::Client::new()
        .get("http://localhost:3001/header")
        .header("x-request-id", "bad")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    // error!("{}", response);
}

#[instrument]
async fn _auth(
    headers: HeaderMap,
    mut req: axum::extract::Request,
    next: Next,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if let Some(header) = headers.get("x-request-id") {
        let header = header.to_str().unwrap();
        info!("received header: {}", header);
        if header == "1234" {
            req.extensions_mut().insert(AuthHeader {
                id: header.to_string(),
            });
            info!("valid header");
            return Ok(next.run(req).await);
        }
    }
    error!("invalid header");
    Err((StatusCode::UNAUTHORIZED, "invalid header".to_string()))
}

#[utoipa::path(
    get,
    path = "/warandpeace",
    responses(
        (status = 200, description = "Display War and Peace", body = [String])
    )
)]
async fn war_and_peace_handler() -> impl IntoResponse {
    const WAR_AND_PEACE: &str = include_str!("war_and_peace.txt");
    Html(WAR_AND_PEACE)
}

fn init_tracer(otlp_endpoint: &str) -> Result<sdktrace::Tracer, TraceError> {
    opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(otlp_endpoint),
        )
        .with_trace_config(
            sdktrace::config().with_resource(Resource::new(vec![KeyValue::new(
                "service.name",
                "http_server",
            )])),
        )
        .install_batch(runtime::Tokio)
}

fn init_metrics(otlp_endpoint: &str) -> opentelemetry::metrics::Result<MeterProvider> {
    let export_config = ExportConfig {
        endpoint: otlp_endpoint.to_string(),
        ..ExportConfig::default()
    };
    opentelemetry_otlp::new_pipeline()
        .metrics(runtime::Tokio)
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_export_config(export_config),
        )
        .with_resource(Resource::new(vec![KeyValue::new(
            opentelemetry_semantic_conventions::resource::SERVICE_NAME,
            "http_server",
        )]))
        .build()
}

fn init_logs(otlp_endpoint: &str) -> Result<opentelemetry_sdk::logs::Logger, LogError> {
    opentelemetry_otlp::new_pipeline()
        .logging()
        .with_log_config(
            Config::default().with_resource(Resource::new(vec![KeyValue::new(
                opentelemetry_semantic_conventions::resource::SERVICE_NAME,
                "http_server",
            )])),
        )
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(otlp_endpoint.to_string()),
        )
        .install_batch(runtime::Tokio)
}

// RUST_LOG=debug cargo run
