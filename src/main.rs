use axum::extract::Path;
use axum::{Router, response::Html, routing::get};

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(handler))
        .route("/book/{id}", get(path_extract));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3001")
        .await
        .unwrap();

    println!("Listening on 127.0.0.1:3001");
    axum::serve(listener, app).await.unwrap();
}

async fn handler() -> Html<&'static str> {
    Html("<h1>Hello World</h1>")
}

async fn path_extract(Path(id): Path<u32>) -> Html<String> {
    Html(format!("Hello, {id}!"))
}
