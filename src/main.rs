mod config;
mod errors;
mod handler;
use anyhow::Result;
use axum::{
    extract::Request,
    http::{self, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use config::CONFIG;
use tower_http::{compression::CompressionLayer, cors::CorsLayer, trace::TraceLayer};
use tracing::Level;

#[tokio::main]
async fn main() -> Result<()> {
    configure_tracing();

    let app = router()?;

    #[cfg(any(debug_assertions, feature = "ecs"))]
    axum_http(app).await?;

    #[cfg(feature = "lambda")]
    lambda_http(app).await?;

    Ok(())
}

#[cfg(any(debug_assertions, feature = "ecs"))]
async fn axum_http(app: Router) -> Result<()> {
    let addr = std::net::SocketAddr::from(([0, 0, 0, 0], config::CONFIG.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    println!("🚀 listening on: {}", addr);

    Ok(())
}

#[cfg(feature = "lambda")]
async fn lambda_http(app: Router) -> Result<()> {
    println!("🚀 starting lambda http ...");
    let app = tower::ServiceBuilder::new()
        .layer(axum_aws_lambda::LambdaLayer::default())
        .service(app);

    lambda_http::run(app)
        .await
        .map_err(|err| anyhow::anyhow!(err))?;

    Ok(())
}

fn router() -> Result<Router> {
    let app = routes()
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .layer(CompressionLayer::new().gzip(true).deflate(true))
        .route_layer(middleware::from_fn(auth));

    Ok(app)
}

fn routes() -> Router {
    use handler::*;

    Router::new()
        .route("/healthz", get(health_check))
        .route("/api/test-chrome", get(test_handler))
        .route("/api/payment-page", post(get_payment_page_handler))
        .route("/api/search-companies", post(get_companies_list_handler))
        .route("/api/registries/:search_keyword", get(registries_get))
        .route("/api/registry/request", post(registry_request))
        .route(
            "/api/registry/request_by_name",
            post(registry_request_by_name),
        )
        .route("/api/corporation/:id", get(corporation_get))
}

fn configure_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter({
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(Level::INFO.into())
                .from_env()
                .unwrap()
        })
        .compact()
        .with_target(false)
        .without_time()
        .init();
}

async fn auth(req: Request, next: Next) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get(http::header::AUTHORIZATION)
        .and_then(|header| header.to_str().ok());

    if let Some(auth_header) = auth_header {
        if auth_header == CONFIG.token {
            return Ok(next.run(req).await);
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}
