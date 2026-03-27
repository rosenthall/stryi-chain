use crate::api_client::{NodeClient, TransactionQueryResponse, TransactionQueryStatus};
use anyhow::Result;
use colored::Colorize;
use std::io::{Write, stdout};
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
    let mut tick = 0usize;

    loop {
        let dots = ".".repeat((tick % 3) + 1);
        print!(
            "\r  Status       {}{}   ",
            "Awaiting confirmation".yellow().bold(),
            dots.yellow()
        );
        stdout().flush()?;

        let response = match client.get_transaction(&tx_hash_str).await {
            Ok(response) => response,
            Err(e) if is_transaction_not_found(&e) => {
                if started.elapsed() >= TX_CONFIRMATION_TIMEOUT {
                    println!();
                    anyhow::bail!(
                        "timed out waiting for transaction {} confirmation after {:?}",
                        tx_hash,
                        TX_CONFIRMATION_TIMEOUT
                    );
                }
                sleep(TX_CONFIRMATION_POLL_INTERVAL).await;
                tick += 1;
                continue;
            }
            Err(e) => {
                println!();
                return Err(anyhow::anyhow!(
                    "failed to fetch transaction {} while waiting for confirmation: {e}",
                    tx_hash
                ));
            }
        };

        if response.status == TransactionQueryStatus::Confirmed {
            print!(
                "\r  Status       {}                     \n",
                "Confirmed!".green().bold()
            );
            stdout().flush()?;
            return Ok(response);
        }

        if started.elapsed() >= TX_CONFIRMATION_TIMEOUT {
            println!();
            anyhow::bail!(
                "timed out waiting for transaction {} confirmation after {:?}",
                tx_hash,
                TX_CONFIRMATION_TIMEOUT
            );
        }

        sleep(TX_CONFIRMATION_POLL_INTERVAL).await;
        tick += 1;
    }
}

fn is_transaction_not_found(error: &anyhow::Error) -> bool {
    error.to_string().contains("returned 404")
}
