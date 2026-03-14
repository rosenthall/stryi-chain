//! An implementation of node's http api.
//! this HTTP server exposes high-level api for the users of blockchain, such as:
//! - Submitting a transaction for inclusion in the next block;
//! - Querying a block by height or hash;
//! - Calculating someone's available balance by address;

mod address;
mod blocks;
mod error;
mod misc;
mod model;
mod tx;

use crate::error::StryiNodeError;
use crate::http::address::__path_get_balance;
use crate::http::blocks::__path_get_block;
use crate::http::error::StryiNodeHttpApiError;
use crate::http::misc::__path_get_nodestate;
use crate::http::misc::get_nodestate;
use crate::http::model::AddressBalanceResponse;
use crate::http::model::BlockResponse;
use crate::http::model::NodeStateBody;
use crate::http::model::SendTransactionRequest;
use crate::http::model::TransactionQueryResponse;
use crate::http::model::UtxoEntry;
use crate::http::tx::__path_send_tx;
use crate::http::tx::ConfirmedTransactionLookup;
use crate::http::tx::get_tx;
use crate::http::tx::send_tx;
use crate::middleware::ready::{NotReadyResponder, ReadyFlag, ReadyGateLayer};
use axum::Router;
use axum::body::Body;
use axum::response::Response;
use axum::routing::{Route, get, post};
use http::StatusCode;
use std::net::SocketAddr;
use std::sync::Arc;
use stryi_core::mempool::MemPool;
use stryi_core::storage::{BlockStorage, StorageStats, UtxoStorage};
use stryi_core::transactions::Transaction;
use stryi_network::{NetworkCommand, PeerId};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, mpsc};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::trace::TraceLayer;
use tracing::info;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

/// Fixed value for the http service name to register in the network.
pub const HTTP_SERVICE_TAG: &str = "http-user";

/// Thin wrapper around the network command channel.
/// Only exposes transaction publishing, keeping HTTP decoupled from irrelevant stryi network's stuff
#[derive(Clone)]
pub struct TxBroadcaster {
    net_cmd: mpsc::Sender<NetworkCommand>,
}

impl TxBroadcaster {
    pub fn new(net_cmd: mpsc::Sender<NetworkCommand>) -> Self {
        Self { net_cmd }
    }

    pub async fn publish_tx(&self, tx: Transaction) {
        let (respond_to, rx) = tokio::sync::oneshot::channel();
        if let Err(e) = self
            .net_cmd
            .send(NetworkCommand::PublishTransaction {
                transaction: tx,
                respond_to,
            })
            .await
        {
            tracing::warn!("Failed to send publish-tx command to network: {e}");
            return;
        }
        if let Ok(Err(e)) = rx.await {
            tracing::warn!("Network failed to publish transaction: {e}");
        }
    }
}

#[derive(Clone)]
pub struct StryiHttpService<DB>
where
    DB: BlockStorage + UtxoStorage + StorageStats,
{
    /// Configuration for this HTTP service
    pub(crate) config: StryiHttpServiceConfig,

    pub(crate) mempool: Arc<RwLock<MemPool>>,
    pub(crate) storage: Arc<RwLock<DB>>,

    /// Broadcaster for gossiping accepted transactions to peers.
    pub(crate) tx_broadcaster: TxBroadcaster,
}

#[derive(Clone, Debug)]
pub struct StryiHttpServiceConfig {
    pub(crate) address: SocketAddr,

    /// Name of this exact chain
    pub(crate) chain_name: String,

    /// PeerID of the node which hosts this http service.
    /// This value will be added to each response header so user can identify peer.
    pub(crate) peer_id: PeerId,

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
            .status(StatusCode::SERVICE_UNAVAILABLE) // 503
            .header("content-type", "application/json")
            .body(Body::empty()) // empty body
            .unwrap()
    }
}

/// Aggregate the spec for http server.
#[derive(OpenApi)]
#[openapi(
    paths(get_nodestate, send_tx, get_block, get_balance),
    components(schemas(
        NodeStateBody,
        SendTransactionRequest,
        BlockResponse,
        AddressBalanceResponse,
        UtxoEntry,
        TransactionQueryResponse,
        StryiNodeHttpApiError
    ))
)]
struct ApiDoc;

/// Spawn the HTTP API
/// All routes stay behind `ReadyGateLayer` until somebody flips `*ready.write() = true`.
pub async fn start_http_server<DB>(
    storage: Arc<RwLock<DB>>,
    cfg: StryiHttpServiceConfig,
    mempool: Arc<RwLock<MemPool>>,
    tx_broadcaster: TxBroadcaster,
    ready: ReadyFlag,
    cancel_token: tokio_util::sync::CancellationToken,
) -> Result<(), StryiNodeError>
where
    DB: BlockStorage
        + UtxoStorage
        + StorageStats
        + ConfirmedTransactionLookup
        + Send
        + Sync
        + 'static,
{
    // shared service state
    let svc = StryiHttpService {
        config: cfg.clone(),
        storage,
        mempool,
        tx_broadcaster,
    };

    let state = Arc::new(svc);

    // build the router
    // TODO: Consider using OpenApiRouter instead of regular one
    // TODO: Add new middleware layer for http for owner node identification: Stryi-PeerId, Stryi-Timestamp and Stryi-Response-Signature
    let app = Router::new()
        // -- Router settings --
        // Enable responses responses
        .layer(CompressionLayer::new())
        // High level logging of requests and responses
        .layer(TraceLayer::new_for_http())
        // Readiness gate
        .layer(ReadyGateLayer::new(ready))
        // -- Functional endpoints --
        // docs
        .merge(SwaggerUi::new("/api/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
        .route("/api/nodestate", get(get_nodestate))
        // TODO: Make BlockData.inputs skip serialization of no inputs
        .route("/api/block/{param}", get(blocks::get_block))
        .route("/api/tx/{hash}", get(get_tx))
        .route("/api/tx", post(send_tx))
        .route("/api/address/{addr}/balance", get(address::get_balance))
        .with_state(state);

    let listener = TcpListener::bind(cfg.address)
        .await
        .map_err(|e| StryiNodeError::HttpServer(e.to_string()))?;

    info!("HTTP API listening on {}", cfg.address);

    // run server
    axum::serve(listener, ServiceBuilder::new().service(app))
        .with_graceful_shutdown(cancel_token.cancelled_owned())
        .await
        .map_err(|e| StryiNodeError::HttpServer(e.to_string()))
}
