//! An implementation of node's http api.
//! this HTTP server exposes high-level api for the users of blockchain, such as:
//! - Submitting a transaction for inclusion in the next block;
//! - Querying a block by height or hash;
//! - Calculating someone's available balance by address;

mod model;
mod misc;

use crate::http::misc::__path_get_nodestate;
use crate::http::model::NodeStateBody;
use crate::http::misc::{get_nodestate};
use std::net::SocketAddr;
use std::sync::Arc;
use axum::body::Body;
use axum::response::Response;
use axum::Router;
use axum::routing::{get, Route};
use http::StatusCode;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tower::ServiceBuilder;
use tracing::info;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use crate::error::StryiNodeError;
use crate::middleware::{NotReadyResponder, ReadyFlag, ReadyGateLayer};

#[derive(Clone)]
pub struct StryiHttpService<DB>
where DB:
BlockStorage + UtxoStorage + StorageStats
{
    pub(crate) config : StryiHttpServiceConfig,

    /// Arc'd storage reference
    pub(crate) storage : Arc<RwLock<DB>>,
}



#[derive(Clone, Debug)]
pub struct StryiHttpServiceConfig {
    pub(crate) address : SocketAddr,

    /// Name of this exact chain
    pub(crate) chain_name: String,

    /// Numeric version of this http API
    pub(crate) api_version: u32,
}



/// 503 responder for any Axum route
impl<S> NotReadyResponder for Route<S>
where
    S: Clone + Send + Sync + 'static,
{
    type NotReadyResponse = Response<Body>;

    fn not_ready(&self) -> Self::NotReadyResponse {
        Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)// 503
            .header("content-type", "application/json")
            .body(Body::empty()) // empty body
            .unwrap()
    }
}

/// Aggregate the spec for http server.
#[derive(OpenApi)]
#[openapi(
    paths(get_nodestate),
    components(schemas(NodeStateBody)))
]
struct ApiDoc;


/// Spawn the HTTP API. All routes stay behind `ReadyGateLayer` until the
/// sync code flips `*ready.write() = true`.
pub async fn start_http_server<DB>(
    storage: Arc<RwLock<DB>>,
    cfg: StryiHttpServiceConfig,
    ready: ReadyFlag,
) -> Result<(), StryiNodeError>
where
    DB: BlockStorage + UtxoStorage + StorageStats +  Send + Sync + 'static,
{
    // shared service state
    let svc = StryiHttpService {
        config: cfg.clone(),
        storage,
    };


    let state = Arc::new(svc);

    // build the router
    let app = Router::new()
        .route("/nodestate", get(get_nodestate::<DB>))
        .merge(SwaggerUi::new("/swagger-ui")
            .url("/api-docs/openapi.json", ApiDoc::openapi()))
        .with_state(state)
        .layer(ReadyGateLayer::new(ready)); // readiness gate

    // bind listener
    let listener = TcpListener::bind(cfg.address)
        .await
        .map_err(|e| StryiNodeError::HttpServer(e.to_string()))?;

    info!("HTTP API listening on {}", cfg.address);

    // run server; axum::serve returns io::Result<()>
    axum::serve(listener, ServiceBuilder::new().service(app))
        .await
        .map_err(|e| StryiNodeError::HttpServer(e.to_string()))
}