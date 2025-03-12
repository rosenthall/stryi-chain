use std::collections::HashMap;
use std::path::PathBuf;
use ractor::{Actor, ActorProcessingErr, ActorRef, Message};
use tokio::sync::{oneshot, RwLock};
use stryi_core::block::{Block, BlockHash};
use stryi_core::transactions::{OutPoint, UTXO};
use crate::{StryiStorage, StryiStorageError};

pub struct StryiStorageActor;



/// StorageCommand defines all the operations that the StorageActor can perform.
/// Each variant includes an oneshot channel to return the result.
pub enum StryiStorageActorMessage {
    /// Retrieve a block by its hash.
    /// The actor will respond with a Result containing the Block or an error.
    GetBlockByHash {
        hash: BlockHash,
        resp: oneshot::Sender<Result<Block, StryiStorageError>>,
    },

    /// Insert a block into storage.
    /// The actor will respond with a Result<(), StryiStorageError>.
    PutBlock {
        block: Block,
        resp: oneshot::Sender<Result<(), StryiStorageError>>,
    },

    /// Retrieve a block by its height.
    /// The actor will respond with a Result containing the Block or an error.
    GetBlockByHeight {
        height: usize,
        resp: oneshot::Sender<Result<Block, StryiStorageError>>,
    },

    /// Retrieve UTXO(s) for one or more outpoints.
    /// This single message is designed to cover both single and batch queries.
    /// The actor will respond with a HashMap mapping each requested OutPoint to its corresponding UTXO.
    GetUtxos {
        outpoints: Vec<OutPoint>,
        resp: oneshot::Sender<Result<HashMap<OutPoint, UTXO>, StryiStorageError>>,
    },

    /// Insert a UTXO for a given outpoint.
    /// The actor will respond with a Result<(), StryiStorageError>.
    PutUtxo {
        outpoint: OutPoint,
        utxo: UTXO,
        resp: oneshot::Sender<Result<(), StryiStorageError>>,
    },

    /// Remove the UTXO associated with the given outpoint.
    /// The actor will respond with a Result<(), StryiStorageError>.
    RemoveUtxo {
        outpoint: OutPoint,
        resp: oneshot::Sender<Result<(), StryiStorageError>>,
    },
}



#[ractor::async_trait]
impl Actor for StryiStorageActor {

    /// The actor's state is just RwLock that owns StryiStorage
    type State = RwLock<StryiStorage>;
    type Msg = StryiStorageActorMessage;
    type Arguments = PathBuf;

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        arguments: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {

        // Initialize StryiStorage using the provided path.
        let storage = StryiStorage::initialize_in_path(arguments)
            .map_err(|e| ActorProcessingErr::from(e.to_string()))?;

        // Wrap storage into RwLock and set as actor state.
        Ok(RwLock::new(storage))

    }
    
    async fn handle(&self, _myself: ActorRef<Self::Msg>,
                    message: Self::Msg,
                    state: &mut Self::State)
                    -> Result<(), ActorProcessingErr> {
        match message {
            _ => todo!()
        } 
    }
}
