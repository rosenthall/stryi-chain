//! An implementation of node's http api.
//! this HTTP server exposes high-level api for the users of blockchain, such as: 
//! - Submitting a transaction for inclusion in the next block; 
//! - Querying a block by height or hash;
//! - Calculating someone's available balance by address;

use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use stryi_core::storage::{BlockStorage, UtxoStorage};

#[derive(Clone)]
pub struct StryiHttpService<DB>
where DB: 
     BlockStorage + UtxoStorage 
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
    pub(crate) api_version: usize,
}


// TODO: Setup basic HTTP-service via axum and tower, and also implement NotReadyResponder trait

