use crate::chaingen::state::GenerationState;
use crate::chaingen::utxo::{TransactionPattern, UtxoInfo};
use k256::ecdsa::SigningKey;
use rand::Rng;
use rand::prelude::IndexedRandom;
use stryi_core::address::AccountAddress;
use stryi_core::transactions::{
    FeePolicy, OutPoint, Transaction, TransactionData, TransactionIn, TransactionKind,
    TransactionOut, estimate_transaction_size,
};
use stryi_core::{PrivateKey, StryiCoreError};
use tracing::debug;

/// FundAccount represents account, that will distribute own balance to other accounts
/// for generating purposes.
/// The structure is :
/// `AccountAddress` for address
/// `PrivateKey` for pk. that corresponds to .0
/// `u64` for available balance
/// `OutPoint` for exact outpoint to spend for distribution.
pub type FundAccount = (AccountAddress, PrivateKey, u64, OutPoint);

/// helper function for splitting balance.
fn calculate_balance_per_account(total_balance: u64, account_num: usize) -> u64 {
    let after_fee = total_balance * 99 / 100;
    let per_account = after_fee / account_num as u64;

    // Round down to nearest 10
    (per_account / 10) * 10
}

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

/// Generates transaction that evenly distributes balance from `generation_state`'s fund_account
pub fn generate_distributing_transaction(
    generation_state: &mut GenerationState,
) -> Result<Transaction, StryiCoreError> {
    // no outputs must exist before dist. tx
    if !generation_state.account_utxos.is_empty() {
        return Err(StryiCoreError::other(
            "No UTXOs on generated addresses must be present before the distribution block!",
        ));
    }

    let (addr, signing_key, balance, outpoint) = generation_state.fund_account.clone();
    let account_num = generation_state.accounts.len();

    let balance_per_account = calculate_balance_per_account(balance, account_num);

    debug!(address = ?addr.to_string(), ?balance, ?account_num, ?balance_per_account);

    // make sure we have some leftover balance to pay fee
    assert!(balance > balance_per_account * account_num as u64);

    // generate outputs
    let outputs: Vec<TransactionOut> = generation_state
        .accounts
        .keys()
        .map(|addr| TransactionOut {
            value: balance_per_account,
            recipient: *addr,
        })
        .collect();

    // build tx data
    let data = TransactionData {
        version: 0,
        kind: TransactionKind::Payment,
        inputs: vec![TransactionIn {
            previous_output: outpoint,
            sequence: 0,
        }],
        outputs,
    };

    Ok(data.sign(&signing_key.into_inner().clone()))
}

/// Pattern generators return transaction data + outputs (already done in _generate_simple_tx)
fn generate_transaction_by_pattern(
    pattern: TransactionPattern,
    sender: AccountAddress,
    utxos_to_spend: Vec<UtxoInfo>,
    receiver_pool: &[AccountAddress],
    rng: &mut impl Rng,
    params: &TransactionGenerationParams,
) -> Option<(TransactionData, Vec<TransactionOut>)> {
    match pattern {
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
            generate_splitting_tx(sender, utxo, receiver_pool, rng, params) // generate_splitting_tx(sender, utxos_to_spend, receiver_pool, rng, params)
        }
        TransactionPattern::Complex => {
            // Complex needs multiple UTXOs and creates multiple outputs
            if utxos_to_spend.len() < 2 {
                return None;
            }
            generate_complex_tx(sender, utxos_to_spend, receiver_pool, rng, params)
        }
    }
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

// Main entry point: build tx and update state
#[allow(clippy::too_many_arguments)]
pub fn generate_transaction(
    generation_state: &mut GenerationState,
    sender: AccountAddress,
    signing_key: &SigningKey,
    pattern: TransactionPattern,
    utxos_to_spend: Vec<UtxoInfo>,
    receiver_pool: &[AccountAddress],
    rng: &mut impl Rng,
    params: &TransactionGenerationParams,
    current_height: u64,
) -> Option<Transaction> {
    // Generate the transaction
    let (tx_data, outputs) = generate_transaction_by_pattern(
        pattern,
        sender,
        utxos_to_spend.clone(),
        receiver_pool,
        rng,
        params,
    )?;

    // Sign it
    let transaction = tx_data.sign(signing_key);
    let txid = transaction.data.hash();

    // Update state: the inputs are in utxos_to_spend, outputs are in the transaction
    generation_state.spend_utxos(&sender, &utxos_to_spend);

    for (vout, output) in outputs.iter().enumerate() {
        let new_utxo = UtxoInfo {
            outpoint: OutPoint {
                txid,
                vout: vout as u32,
            },
            value: output.value,
            height_created: current_height,
            is_coinbase: false,
        };
        generation_state.add_utxo(output.recipient, new_utxo);
    }

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
        return None;
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

/// Split one UTXO into multiple outputs
fn generate_splitting_tx(
    _sender: AccountAddress,
    _utxo_to_spend: UtxoInfo, // Single UTXO
    _receiver_pool: &[AccountAddress],
    _rng: &mut impl Rng,
    _params: &TransactionGenerationParams,
) -> Option<(TransactionData, Vec<TransactionOut>)> {
    todo!("implement splitting tx gen")
}

/// Multiple inputs to multiple outputs
fn generate_complex_tx(
    _sender: AccountAddress,
    _utxos_to_spend: Vec<UtxoInfo>, // Multiple UTXOs
    _receiver_pool: &[AccountAddress],
    _rng: &mut impl Rng,
    _params: &TransactionGenerationParams,
) -> Option<(TransactionData, Vec<TransactionOut>)> {
    todo!("implement complex tx gen")
}
