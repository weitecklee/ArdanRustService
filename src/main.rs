use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::{Extension, Json};
use axum::{Router, response::Html, routing::get};

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

#[tokio::main]
async fn main() {
    let shared_counter = Arc::new(MyCounter {
        counter: AtomicUsize::new(0),
    });

    let shared_text = Arc::new(MyConfig {
        text: "This is my configuration.".to_string(),
    });

    let app = Router::new()
        .nest("/1", service_one())
        .nest("/2", service_two())
        .nest("/counter", counter_sv())
        .route("/", get(handler))
        .route("/book/{id}", get(path_extract))
        .route("/book", get(query_extract))
        .route("/header", get(header_extract))
        .route("/status", get(status_handler))
        .layer(Extension(shared_counter))
        .layer(Extension(shared_text));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3001")
        .await
        .unwrap();

    println!("Listening on 127.0.0.1:3001");
    axum::serve(listener, app).await.unwrap();
}

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

fn service_two() -> Router {
    Router::new().route("/", get(|| async { Html("Service Two".to_string()) }))
}

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

async fn path_extract(Path(id): Path<u32>) -> Html<String> {
    Html(format!("Hello, {id}!"))
}

async fn query_extract(Query(params): Query<HashMap<String, String>>) -> Html<String> {
    Html(format!("{params:#?}"))
}

async fn header_extract(headers: HeaderMap) -> Html<String> {
    Html(format!("{headers:#?}"))
}

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
