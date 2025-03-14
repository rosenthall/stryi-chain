use std::path::PathBuf;
use std::collections::HashMap;
use ractor::{Actor, ActorProcessingErr, ActorRef, Message};
use tokio::sync::{oneshot, RwLock};
use stryi_core::{
    block::{Block, BlockHash},
    transactions::{OutPoint, UTXO},
    storage::{BlockStorage, UtxoStorage, StorageStats},
};
use crate::{StryiStorage, StryiStorageError};
use crate::stats::StorageStateInformation;
use crate::actor::StryiStorageActorMessage::*;

mod impls;
pub use impls::*;

mod proxy;


/// Message enum defining all operations that the actor can perform.
/// Each variant carries an oneshot sender for the reply.
pub enum StryiStorageActorMessage {
    // --- Block operations ---
    GetBlockByHash {
        hash: BlockHash,
        resp: oneshot::Sender<Result<Block, StryiStorageError>>,
    },
    PutBlock {
        block: Block,
        resp: oneshot::Sender<Result<(), StryiStorageError>>,
    },
    GetBlockByHeight {
        height: usize,
        resp: oneshot::Sender<Result<Block, StryiStorageError>>,
    },
    BlockExists {
        hash: BlockHash,
        resp: oneshot::Sender<Result<bool, StryiStorageError>>,
    },
    GetChain {
        resp: oneshot::Sender<Result<Vec<Block>, StryiStorageError>>,
    },
    GetRange {
        start_height: usize,
        end_height: usize,
        resp: oneshot::Sender<Result<Vec<Block>, StryiStorageError>>,
    },
    // --- UTXO operations ---
    GetUtxo {
        outpoint: OutPoint,
        resp: oneshot::Sender<Result<UTXO, StryiStorageError>>,
    },
    GetUtxos {
        outpoints: Vec<OutPoint>,
        resp: oneshot::Sender<Result<HashMap<OutPoint, UTXO>, StryiStorageError>>,
    },
    GetUtxosForAddress {
        address: stryi_core::address::AccountAddress,
        resp: oneshot::Sender<Result<Vec<(OutPoint, UTXO)>, StryiStorageError>>,
    },
    PutUtxo {
        outpoint: OutPoint,
        utxo: UTXO,
        resp: oneshot::Sender<Result<(), StryiStorageError>>,
    },
    RemoveUtxo {
        outpoint: OutPoint,
        resp: oneshot::Sender<Result<(), StryiStorageError>>,
    },
    BatchPutUtxos {
        utxos: Vec<(OutPoint, UTXO)>,
        resp: oneshot::Sender<Result<(), StryiStorageError>>,
    },
    BatchRemoveUtxos {
        outpoints: Vec<OutPoint>,
        resp: oneshot::Sender<Result<(), StryiStorageError>>,
    },
    // --- Storage stats operation ---
    GetStorageStats {
        resp: oneshot::Sender<Result<StorageStateInformation, StryiStorageError>>,
    },
    // --- Reorganization ---
    ReorganizeChain {
        old_tip: BlockHash,
        new_tip: BlockHash,
        resp: oneshot::Sender<Result<(), StryiStorageError>>,
    },
}

/// Actor that encapsulates a StryiStorage instance wrapped in an RwLock.
pub struct StryiStorageActor;

#[ractor::async_trait]
impl Actor for StryiStorageActor {
    type State = RwLock<StryiStorage>;
    type Msg = StryiStorageActorMessage;
    type Arguments = PathBuf;

    // Pre-start hook: initialize storage from the provided in arguments path.
    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        arguments: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
     
        let storage = StryiStorage::initialize_in_path(arguments)
            .map_err(|e| ActorProcessingErr::from(e.to_string()))?;
     
        Ok(RwLock::new(storage))
    }

    // Handle each incoming message by reading or writing the storage.
    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        message: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match message {
            // ----- BLOCK OPERATIONS -----
            GetBlockByHash { hash, resp } => {
                let storage = state.read().await;
                let result = storage.get_block_by_hash(hash).await;
                let _ = resp.send(result);
            },
            PutBlock { block, resp } => {
                let mut storage = state.write().await;
                let result = storage.put_block(&block).await;
                let _ = resp.send(result);
            },
            GetBlockByHeight { height, resp } => {
                let storage = state.read().await;
                let result = storage.get_block_by_height(height).await;
                let _ = resp.send(result);
            },
            BlockExists { hash, resp } => {
                let storage = state.read().await;
                let result = storage.block_exists(hash).await;
                let _ = resp.send(result);
            },
            GetChain { resp } => {
                let storage = state.read().await;
                let result = storage.get_chain().await;
                let _ = resp.send(result);
            },
            GetRange { start_height, end_height, resp } => {
                let storage = state.read().await;
                let result = storage.get_range(start_height, end_height).await;
                let _ = resp.send(result);
            },
            
            
            // ----- UTXO OPERATIONS -----
            GetUtxo { outpoint, resp } => {
                let storage = state.read().await;
                let result = storage.get_utxo(&outpoint).await;
                let _ = resp.send(result);
            },
            GetUtxos { outpoints, resp } => {
                let storage = state.read().await;
                let result = storage.get_utxos(&outpoints.into_iter().collect()).await;
                let _ = resp.send(result);
            },
            GetUtxosForAddress { address, resp } => {
                let storage = state.read().await;
                let result = storage.get_utxos_for_address(&address).await;
                let _ = resp.send(result);
            },
            PutUtxo { outpoint, utxo, resp } => {
                let mut storage = state.write().await;
                let result = storage.put_utxo(&outpoint, utxo).await;
                let _ = resp.send(result);
            },
            RemoveUtxo { outpoint, resp } => {
                let mut storage = state.write().await;
                let result = storage.remove_utxo(&outpoint).await;
                let _ = resp.send(result);
            },
            BatchPutUtxos { utxos, resp } => {
                let mut storage = state.write().await;
                let result = storage.batch_put_utxos(utxos).await;
                let _ = resp.send(result);
            },
            BatchRemoveUtxos { outpoints, resp } => {
                let mut storage = state.write().await;
                let result = storage.batch_remove_utxos(outpoints).await;
                let _ = resp.send(result);
            },
            
            
            
            
            // --- STORAGE STATS ---
            GetStorageStats { resp } => {
                let storage = state.read().await;
                let result = storage.get_current_storage_state();
                let _ = resp.send(result);
            },
            
            // --- Reorganization ---
            ReorganizeChain { old_tip, new_tip, resp } => {
                // Acquire write lock to ensure exclusive access during reorg.
                let mut storage = state.write().await;
                let result = crate::reorganization::ChainReorganizer::new(&mut storage)
                    .reorganize(old_tip, new_tip)
                    .await;
                let _ = resp.send(result);
            },
            
            
            
        }
        Ok(())
    }
}
