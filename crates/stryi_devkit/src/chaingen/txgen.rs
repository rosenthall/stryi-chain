use crate::chaingen::state::GenerationState;
use crate::chaingen::utxo::{TransactionPattern, UtxoInfo};
use k256::ecdsa::SigningKey;
use rand::Rng;
use rand::seq::IndexedRandom;
use stryi_core::StryiCoreError;
use stryi_core::address::AccountAddress;
use stryi_core::transactions::{
    FeePolicy, Transaction, TransactionData, TransactionIn, TransactionKind, TransactionOut,
    estimate_transaction_size,
};
use tracing::{debug, info};

/// Transaction generation parameters that affect UTXO complexity
#[derive(Clone, Debug)]
pub struct TransactionGenerationParams {
    /// Minimum viable output value
    pub min_output_value: u64,
    /// Consts values to calculate required fees.
    pub fee_policy: FeePolicy,
    /// Maximum inputs in a transaction
    pub max_inputs: usize,
    /// Maximum outputs in a transaction
    pub max_outputs: usize,
}

impl Default for TransactionGenerationParams {
    fn default() -> Self {
        Self {
            min_output_value: 10_000,
            fee_policy: FeePolicy::default(),
            max_inputs: 8,
            max_outputs: 8,
        }
    }
}

pub fn generate_distributing_transaction(
    generation_state: &mut GenerationState,
) -> Result<Transaction, StryiCoreError> {
    // Check that no UTXOs exist on generated addresses yet
    if !generation_state.account_utxos.is_empty() {
        return Err(StryiCoreError::other(
            "No UTXOs on generated addresses must be present before the distribution block",
        ));
    }

    // Extract fund account details
    let fund_account = generation_state.fund_account.clone();
    let funder_address = fund_account.address();
    let private_key = fund_account.private_key();
    let utxos = fund_account.utxos();
    let total_balance = fund_account.total_balance();

    let account_num = generation_state.accounts.len();
    let num_inputs = utxos.len();
    let num_outputs = account_num;

    info!(
        funder = %funder_address,
        total_balance = total_balance,
        accounts = account_num,
        utxo_count = num_inputs,
        "Starting fund distribution"
    );

    // Calculate fees
    let fee_policy = FeePolicy::default();
    let actual_fee = estimate_fee(num_inputs, num_outputs, &fee_policy);

    debug!(
        num_inputs = num_inputs,
        num_outputs = num_outputs,
        actual_fee = actual_fee,
        "Calculated distribution transaction fee"
    );

    // Verify we have enough balance to cover the fee
    if total_balance <= actual_fee {
        return Err(StryiCoreError::other(
            "Insufficient balance to cover transaction fee",
        ));
    }

    // Calculate distributable amount
    let distributable = total_balance - actual_fee;
    let mut balance_per_account = distributable / account_num as u64;

    // Round down to nearest 10 for cleaner numbers
    balance_per_account = (balance_per_account / 10) * 10;

    let distributed = balance_per_account * account_num as u64;
    let leftover = distributable - distributed;

    let min_output_value = TransactionGenerationParams::default().min_output_value;
    if balance_per_account < min_output_value {
        return Err(StryiCoreError::other(
            "Distribution amount per account below minimum viable output",
        ));
    }

    debug!(
        "FUND UTXOS: {:?}",
        fund_account
            .utxos()
            .keys()
            .map(|op| (op.txid, op.vout))
            .collect::<Vec<_>>()
    );

    // Inputs - canonical order (txid, vout)
    let mut inputs: Vec<TransactionIn> = utxos
        .keys()
        .map(|outpoint| TransactionIn {
            previous_output: *outpoint,
            sequence: 0,
        })
        .collect();

    inputs.sort_by(|a, b| {
        a.previous_output
            .txid
            .data
            .cmp(&b.previous_output.txid.data)
            .then(a.previous_output.vout.cmp(&b.previous_output.vout))
    });

    // Outputs - canonical order (recipient)
    let mut outputs: Vec<TransactionOut> = generation_state
        .accounts
        .keys()
        .map(|addr| TransactionOut {
            value: balance_per_account,
            recipient: *addr,
        })
        .collect();

    outputs.sort_by(|a, b| a.recipient.data.cmp(&b.recipient.data));

    // Add leftover AFTER sorting
    if leftover > 0 && !outputs.is_empty() {
        outputs[0].value += leftover;
    }

    let data = TransactionData {
        version: 0,
        kind: TransactionKind::Payment,
        inputs,
        outputs,
    };

    // Sign transaction
    Ok(data.sign(&private_key.clone().into_inner()))
}

/// Exact fee calculation using accurate size estimation
#[inline]
fn estimate_fee(num_inputs: usize, num_outputs: usize, fee_policy: &FeePolicy) -> u64 {
    let estimated_size = estimate_transaction_size(num_inputs, num_outputs);

    fee_policy.fixed_fee
        + (num_inputs as u64 * fee_policy.input_cost)
        + (num_outputs as u64 * fee_policy.output_cost)
        + (estimated_size as u64 * fee_policy.byte_cost)
}

/// Calculate minimum economically viable UTXO value.
/// A UTXO is economically viable if it can cover its own future spending cost
/// plus create at least one meaningful output (min_output_value).
///
/// We add a 50% safety margin to account for:
/// - Potential fee increases
/// - Multiple inputs scenarios
/// - Ensuring outputs remain spendable through multiple generations
#[inline]
fn min_viable_output(params: &TransactionGenerationParams) -> u64 {
    let future_spend_cost = estimate_fee(1, 1, &params.fee_policy);
    let base_minimum = future_spend_cost + params.min_output_value;

    // Add 50% safety margin to prevent gradual UTXO value degradation
    base_minimum + (base_minimum / 2)
}

/// Generates and signs a transaction using the given [`TransactionPattern`].
///
/// This function is **pure**:
/// it does not update UTXO state, balances, or persistence layers.
///
/// Returns `None` if input UTXOs do not satisfy the selected pattern
/// or if transaction generation fails.
pub fn generate_transaction(
    signing_key: &SigningKey,
    pattern: TransactionPattern,
    utxos_to_spend: Vec<UtxoInfo>,
    receiver_pool: &[AccountAddress],
    rng: &mut impl Rng,
    params: &TransactionGenerationParams,
) -> Option<Transaction> {
    // Generate the transaction
    let (tx_data, _outputs) = match pattern {
        TransactionPattern::Simple => {
            // Simple pattern needs exactly 1 UTXO
            let utxo = utxos_to_spend.into_iter().next()?;
            generate_simple_tx(utxo, receiver_pool, rng, params)
        }

        TransactionPattern::Consolidation => {
            // Consolidation needs multiple UTXOs
            if utxos_to_spend.len() < 2 {
                return None;
            }
            generate_consolidation_tx(utxos_to_spend, receiver_pool, rng, params)
        }
        TransactionPattern::Splitting => {
            // Splitting needs 1 UTXO but creates multiple outputs
            let utxo = utxos_to_spend.into_iter().next()?;
            generate_splitting_tx(utxo, receiver_pool, rng, params) // generate_splitting_tx(sender, utxos_to_spend, receiver_pool, rng, params)
        }
        TransactionPattern::Complex => {
            // Complex needs multiple UTXOs and creates multiple outputs
            if utxos_to_spend.len() < 2 {
                return None;
            }
            generate_complex_tx(utxos_to_spend, receiver_pool, rng, params)
        }
    }?;

    // Sign it
    let transaction = tx_data.sign(signing_key);

    Some(transaction)
}

// --- Pattern-Specific Helpers ---

/// Generate simple transaction: 1 input -> 1 output
pub fn generate_simple_tx(
    utxo_to_spend: UtxoInfo,
    receiver_pool: &[AccountAddress],
    rng: &mut impl Rng,
    params: &TransactionGenerationParams,
) -> Option<(TransactionData, Vec<TransactionOut>)> {
    let receiver = *receiver_pool.choose(rng)?;

    let input = TransactionIn {
        previous_output: utxo_to_spend.outpoint,
        sequence: 0,
    };

    // Calculate fee for single input and single output
    let fee = estimate_fee(1, 1, &params.fee_policy);

    // Check if UTXO is economically spendable
    if utxo_to_spend.value <= fee {
        debug!(
            "UTXO value {} not enough to cover fee {}",
            utxo_to_spend.value, fee
        );
    }

    // Calculate output value (entire UTXO minus fee)
    let output_value = utxo_to_spend.value - fee;

    // Ensure output meets minimum value requirement
    if output_value < params.min_output_value {
        debug!(
            "Output value too small: {} < {}",
            output_value, params.min_output_value
        );
        return None;
    }

    let outputs = vec![TransactionOut {
        value: output_value,
        recipient: receiver,
    }];

    let tx_data = TransactionData {
        version: 0,
        kind: TransactionKind::Payment,
        inputs: vec![input],
        outputs: outputs.clone(),
    };

    Some((tx_data, outputs))
}

/// Generate consolidation transaction: N inputs -> 1 output
/// Combines multiple small UTXOs into one larger UTXO
pub fn generate_consolidation_tx(
    utxos_to_spend: Vec<UtxoInfo>,
    receiver_pool: &[AccountAddress],
    rng: &mut impl Rng,
    params: &TransactionGenerationParams,
) -> Option<(TransactionData, Vec<TransactionOut>)> {
    if utxos_to_spend.len() < 2 {
        debug!("Need at least 2 UTXOs for consolidation");
        return None;
    }

    // Limit number of inputs to prevent huge transactions
    let utxos_to_use: Vec<_> = utxos_to_spend.into_iter().take(params.max_inputs).collect();

    let num_inputs = utxos_to_use.len();
    let receiver = *receiver_pool.choose(rng)?;

    let inputs: Vec<TransactionIn> = utxos_to_use
        .iter()
        .map(|utxo| TransactionIn {
            previous_output: utxo.outpoint,
            sequence: 0,
        })
        .collect();

    let total_input: u64 = utxos_to_use.iter().map(|u| u.value).sum();

    // Calculate exact fee for N inputs -> 1 output
    let fee = estimate_fee(num_inputs, 1, &params.fee_policy);

    debug!(
        "Consolidating {} UTXOs (total: {}, fee: {})",
        num_inputs, total_input, fee
    );

    if total_input <= fee {
        debug!(
            "Total input {} not enough to cover fee {}",
            total_input, fee
        );
        return None;
    }

    let output_value = total_input - fee;
    let min_viable = min_viable_output(params);

    // Ensure consolidated output is economically viable
    if output_value < min_viable {
        debug!(
            "Consolidation result too small: {} < {}",
            output_value, min_viable
        );
        return None;
    }

    let outputs = vec![TransactionOut {
        value: output_value,
        recipient: receiver,
    }];

    let tx_data = TransactionData {
        version: 0,
        kind: TransactionKind::Payment,
        inputs,
        outputs: outputs.clone(),
    };

    Some((tx_data, outputs))
}

fn generate_splitting_tx(
    utxo_to_spend: UtxoInfo,
    receiver_pool: &[AccountAddress],
    rng: &mut impl Rng,
    params: &TransactionGenerationParams,
) -> Option<(TransactionData, Vec<TransactionOut>)> {
    let min_viable = min_viable_output(params);

    // Limit splitting to reasonable range (2-4 outputs) to avoid UTXO explosion
    let max_split_outputs = 4.min(params.max_outputs).min(receiver_pool.len());

    // Calculate maximum possible outputs based on available value
    let mut max_possible_outputs = 2;
    for num_outputs in 2..=max_split_outputs {
        let fee = estimate_fee(1, num_outputs, &params.fee_policy);
        if utxo_to_spend.value <= fee {
            break;
        }
        let available = utxo_to_spend.value - fee;
        if available >= min_viable * num_outputs as u64 {
            max_possible_outputs = num_outputs;
        } else {
            break;
        }
    }

    if max_possible_outputs < 2 {
        debug!(
            "UTXO value {} too small to split into multiple viable outputs",
            utxo_to_spend.value
        );
        return None;
    }

    // Choose number of outputs within viable range
    let num_outputs = rng.random_range(2..=max_possible_outputs);

    let input = TransactionIn {
        previous_output: utxo_to_spend.outpoint,
        sequence: 0,
    };

    // Calculate fee for 1 input -> N outputs
    let fee = estimate_fee(1, num_outputs, &params.fee_policy);

    // Check if fee can be covered BEFORE subtracting
    if utxo_to_spend.value <= fee {
        debug!(
            "UTXO value {} cannot cover fee {} for {} outputs",
            utxo_to_spend.value, fee, num_outputs
        );
        return None;
    }

    let available_for_outputs = utxo_to_spend.value - fee;

    // Additional safety check: ensure we can create viable outputs
    if available_for_outputs < min_viable * num_outputs as u64 {
        debug!(
            "Available value {} insufficient for {} viable outputs (need {})",
            available_for_outputs,
            num_outputs,
            min_viable * num_outputs as u64
        );
        return None;
    }

    // Select random receivers
    let receivers: Vec<AccountAddress> = receiver_pool
        .choose_multiple(rng, num_outputs)
        .copied()
        .collect();

    // Split value among outputs with some randomization
    let mut outputs = Vec::with_capacity(num_outputs);
    let mut remaining = available_for_outputs;

    for (i, receiver) in receivers.iter().enumerate() {
        let output_value = if i == num_outputs - 1 {
            // Last output gets all remaining value
            remaining
        } else {
            // Calculate how much we need to reserve for remaining outputs
            let outputs_left = (num_outputs - i - 1) as u64;
            let reserved_for_remaining = min_viable.saturating_mul(outputs_left);

            // Ensure we have enough remaining value
            let available_for_this = remaining.saturating_sub(reserved_for_remaining);
            if available_for_this < min_viable {
                // Not enough left, give minimum viable
                min_viable
            } else {
                let fair_share = remaining / (num_outputs - i) as u64;
                let max_val = fair_share.min(available_for_this);
                if max_val <= min_viable {
                    min_viable
                } else {
                    rng.random_range(min_viable..=max_val)
                }
            }
        };

        outputs.push(TransactionOut {
            value: output_value,
            recipient: *receiver,
        });
        remaining = remaining.saturating_sub(output_value);
    }

    debug!(
        "Splitting UTXO of {} into {} outputs",
        utxo_to_spend.value, num_outputs
    );

    let tx_data = TransactionData {
        version: 0,
        kind: TransactionKind::Payment,
        inputs: vec![input],
        outputs: outputs.clone(),
    };

    Some((tx_data, outputs))
}

/// Generate complex transaction: N inputs -> M outputs
/// Combines multiple UTXOs and distributes to multiple recipients
fn generate_complex_tx(
    utxos_to_spend: Vec<UtxoInfo>,
    receiver_pool: &[AccountAddress],
    rng: &mut impl Rng,
    params: &TransactionGenerationParams,
) -> Option<(TransactionData, Vec<TransactionOut>)> {
    if utxos_to_spend.len() < 2 {
        debug!("Need at least 2 UTXOs for complex transaction");
        return None;
    }

    // Limit inputs to prevent huge transactions
    let utxos_to_use: Vec<_> = utxos_to_spend.into_iter().take(params.max_inputs).collect();
    let num_inputs = utxos_to_use.len();
    let total_input: u64 = utxos_to_use.iter().map(|u| u.value).sum();

    let min_viable = min_viable_output(params);

    // Limit complex outputs to reasonable range (2-5) to avoid UTXO explosion
    let max_complex_outputs = 5.min(params.max_outputs).min(receiver_pool.len());

    // Calculate maximum possible outputs based on available value
    let mut max_possible_outputs = 2;
    for num_outputs in 2..=max_complex_outputs {
        let fee = estimate_fee(num_inputs, num_outputs, &params.fee_policy);
        if total_input <= fee {
            break;
        }
        let available = total_input - fee;
        if available >= min_viable * num_outputs as u64 {
            max_possible_outputs = num_outputs;
        } else {
            break;
        }
    }

    if max_possible_outputs < 2 {
        debug!(
            "Total input {} too small to create multiple viable outputs",
            total_input
        );
        return None;
    }

    // Choose number of outputs within viable range
    let num_outputs = rng.random_range(2..=max_possible_outputs);

    let inputs: Vec<TransactionIn> = utxos_to_use
        .iter()
        .map(|utxo| TransactionIn {
            previous_output: utxo.outpoint,
            sequence: 0,
        })
        .collect();

    // Calculate fee for N inputs -> M outputs
    let fee = estimate_fee(num_inputs, num_outputs, &params.fee_policy);

    debug!(
        "Complex tx: {} inputs (total: {}) -> {} outputs (fee: {})",
        num_inputs, total_input, num_outputs, fee
    );

    let available_for_outputs = total_input - fee;

    // Select random receivers
    let receivers: Vec<AccountAddress> = receiver_pool
        .choose_multiple(rng, num_outputs)
        .copied()
        .collect();

    // Distribute value among outputs with randomization
    let mut outputs = Vec::with_capacity(num_outputs);
    let mut remaining = available_for_outputs;

    for (i, receiver) in receivers.iter().enumerate() {
        let output_value = if i == num_outputs - 1 {
            // Last output gets all remaining value
            remaining
        } else {
            // Calculate how much we need to reserve for remaining outputs
            let outputs_left = (num_outputs - i - 1) as u64;
            let reserved_for_remaining = min_viable.saturating_mul(outputs_left);

            // Ensure we have enough remaining value
            let available_for_this = remaining.saturating_sub(reserved_for_remaining);

            if available_for_this < min_viable {
                // Not enough left, give minimum viable
                min_viable
            } else {
                let fair_share = remaining / (num_outputs - i) as u64;
                let max_val = fair_share.min(available_for_this);

                if max_val <= min_viable {
                    min_viable
                } else {
                    rng.random_range(min_viable..=max_val)
                }
            }
        };

        outputs.push(TransactionOut {
            value: output_value,
            recipient: *receiver,
        });

        remaining = remaining.saturating_sub(output_value);
    }

    let tx_data = TransactionData {
        version: 0,
        kind: TransactionKind::Payment,
        inputs,
        outputs: outputs.clone(),
    };

    Some((tx_data, outputs))
}
