//! An implementation of node's http api.
//! this HTTP server exposes high-level api for the users of blockchain, such as:
//! - Submitting a transaction for inclusion in the next block;
//! - Querying a block by height or hash;
//! - Calculating someone's available balance by address;

mod model;
mod misc;
mod tx;
mod error;
mod blocks;

use crate::http::model::BlockResponse;
use crate::http::model::SendTransactionRequest;
use crate::http::tx::__path_send_tx;
use crate::http::misc::__path_get_nodestate;
use crate::http::blocks::__path_get_block;
use crate::http::error::StryiNodeHttpApiError;
use crate::http::model::NodeStateBody;
use crate::http::misc::{get_nodestate};
use std::net::SocketAddr;
use std::sync::Arc;
use axum::body::Body;
use axum::response::Response;
use axum::Router;
use axum::routing::{get, post, Route};
use http::StatusCode;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tower_http::validate_request::ValidateRequestHeaderLayer;
use tracing::info;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use crate::error::StryiNodeError;
use crate::http::tx::send_tx;
use crate::middleware::{NotReadyResponder, ReadyFlag, ReadyGateLayer};

#[derive(Clone)]
pub struct StryiHttpService<DB>
where DB: BlockStorage + UtxoStorage + StorageStats {
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
    paths(get_nodestate, send_tx, get_block),
    components(schemas(NodeStateBody, SendTransactionRequest, BlockResponse, StryiNodeHttpApiError)))
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
    // TODO: Consider using OpenApiRouter instead of regular one
    let app = Router::new()

        // -- Router settings --

        // Enable responses responses
        .layer(CompressionLayer::new())
        // High level logging of requests and responses
        .layer(TraceLayer::new_for_http())
        // Readiness gate
        .layer(ReadyGateLayer::new(ready))
        // Only accept application/json
        .layer(ValidateRequestHeaderLayer::accept("application/json"))

        // -- Functional endpoints --

        // docs
        .merge(SwaggerUi::new("/api/swagger-ui")
            .url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/api/nodestate", get(get_nodestate))

        // TODO: Make signature and merkle_root_hash serializable as base64, not just bytes arrays for better readability
        // TODO: Make BlockData.inputs skip serialization of no inputs
        .route("/api/block/{param}", get(blocks::get_block))

        .route("/api/tx", post(send_tx))

        .with_state(state);

    let listener = TcpListener::bind(cfg.address).await
        .map_err(|e| StryiNodeError::HttpServer(e.to_string()))?;

    info!("HTTP API listening on {}", cfg.address);




    // run server; axum::serve returns io::Result<()>
    axum::serve(listener, ServiceBuilder::new()
        .service(app))
        .await
        .map_err(|e| StryiNodeError::HttpServer(e.to_string()))

}
