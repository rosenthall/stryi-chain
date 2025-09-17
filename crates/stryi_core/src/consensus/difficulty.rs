use crate::StryiCoreError;
use futures::future::BoxFuture;
use std::sync::Arc;

/// Asynchronous difficulty calculator injected as a function.
///
/// - `&S` is the chain state, may include any required data to calculate difficulty; `usize` is the block height.
/// - HRTB (`for<'a>`) ties the future’s lifetime to the borrow of `&S`.
/// - Returns a `u8` difficulty or `StryiCoreError`.
/// - `Send + Sync + 'static` enables sharing across threads.
/// - `BoxFuture` erases the concrete future type.
pub type DifficultyCalc<S> = Arc<
    dyn for<'a> Fn(&'a S, usize) -> BoxFuture<'a, Result<u8, StryiCoreError>>
        + Send
        + Sync
        + 'static,
>;
