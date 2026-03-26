use crate::api_client::{NodeClient, TransactionQueryResponse, TransactionQueryStatus};
use anyhow::Result;
use std::time::Duration;
use stryi_core::transactions::TransactionHash;
use tokio::time::{Instant, sleep};

const TX_CONFIRMATION_POLL_INTERVAL: Duration = Duration::from_millis(1000);
const TX_CONFIRMATION_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) async fn wait_for_tx_confirmation(
    client: &NodeClient,
    tx_hash: &TransactionHash,
) -> Result<TransactionQueryResponse> {
    let started = Instant::now();
    let tx_hash_str = tx_hash.to_string();

    loop {
        let response = match client.get_transaction(&tx_hash_str).await {
            Ok(response) => response,
            Err(e) if is_transaction_not_found(&e) => {
                if started.elapsed() >= TX_CONFIRMATION_TIMEOUT {
                    anyhow::bail!(
                        "timed out waiting for transaction {} confirmation after {:?}",
                        tx_hash,
                        TX_CONFIRMATION_TIMEOUT
                    );
                }
                sleep(TX_CONFIRMATION_POLL_INTERVAL).await;
                continue;
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "failed to fetch transaction {} while waiting for confirmation: {e}",
                    tx_hash
                ));
            }
        };

        if response.status == TransactionQueryStatus::Confirmed {
            return Ok(response);
        }

        if started.elapsed() >= TX_CONFIRMATION_TIMEOUT {
            anyhow::bail!(
                "timed out waiting for transaction {} confirmation after {:?}",
                tx_hash,
                TX_CONFIRMATION_TIMEOUT
            );
        }

        sleep(TX_CONFIRMATION_POLL_INTERVAL).await;
    }
}

fn is_transaction_not_found(error: &anyhow::Error) -> bool {
    error.to_string().contains("returned 404")
}
