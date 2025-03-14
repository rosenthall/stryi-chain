use std::path::PathBuf;
use ractor::{ActorRef, SpawnErr, concurrency::JoinHandle};
use tokio::sync::oneshot;
use crate::actor::{StryiStorageActor, StryiStorageActorMessage};
use crate::actor::StryiStorageActorMessage::*;
use crate::stats::StorageStateInformation;
use crate::StryiStorageError;

/// Public proxy that implements storage traits by forwarding requests to the actor.
/// It’s meant to be used as the public API.
pub struct StryiStorageProxy {
    actor: ActorRef<StryiStorageActorMessage>,
    handle: JoinHandle<()>,
}

impl StryiStorageProxy {
    /// Creates a new proxy instance by starting the underlying actor.
    pub async fn new(path: PathBuf) -> Result<Self, SpawnErr> {
        let (actor_ref, actor_handle) = ractor::Actor::spawn(None, StryiStorageActor, path).await?;
        Ok(StryiStorageProxy { actor: actor_ref, handle: actor_handle })
    }
    

    /// Helper: sends a GetStorageStats message to the actor and awaits the response.
    pub(crate) async fn get_storage_stats(&self) -> Result<StorageStateInformation, StryiStorageError> {
        let (tx, rx) = oneshot::channel();
        
        self.actor.cast(GetStorageStats { resp: tx }).map_err(StryiStorageError::MessagingError)?;

        rx.await.map_err(|e| StryiStorageError::ChannelReceiveError(format!("{:?}", e)))?
    }
    
    /// Dispatches a message to the actor and awaits the response.
    pub(crate) async fn dispatch<T, F>(&self, f: F) -> Result<T, StryiStorageError>
    where
        T: Send + 'static,
        F: FnOnce(oneshot::Sender<Result<T, StryiStorageError>>) -> StryiStorageActorMessage,
    {
        let (tx, rx) = oneshot::channel();

        self.actor.cast(f(tx)).map_err(StryiStorageError::MessagingError)?;;

        rx.await.unwrap_or_else(|e| Err(StryiStorageError::ChannelReceiveError(format!("{:?}", e))))
    }
}

