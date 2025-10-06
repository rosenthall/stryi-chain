use crate::chaingen::state::GenerationState;
use crate::chaingen::utxo::{TransactionPattern, UtxoInfo};
use k256::ecdsa::SigningKey;
use rand::Rng;
use rand::prelude::IndexedRandom;
use stryi_core::address::AccountAddress;
use stryi_core::transactions::{
    FeeCalculator, FeePolicy, OutPoint, Transaction, TransactionData, TransactionIn,
    TransactionKind, TransactionOut,
};
use stryi_core::{PrivateKey, StryiCoreError};
use tracing::{debug, error};

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
    /// Probability of creating change output when remainder is small
    pub change_output_probability: f64,
}

impl Default for TransactionGenerationParams {
    fn default() -> Self {
        Self {
            min_output_value: 10_000,
            fee_policy: FeePolicy::default(),
            max_inputs: 10,
            max_outputs: 8,
            change_output_probability: 0.8,
        }
    }
}

impl TransactionGenerationParams {
    /// Check if a value is worth creating an output for
    pub fn is_output_worthwhile(&self, value: u64, rng: &mut impl Rng) -> bool {
        if value >= self.min_output_value * 2 {
            true
        } else if value >= self.min_output_value {
            rng.random_bool(self.change_output_probability)
        } else {
            false
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
            generate_simple_tx(sender, utxo, receiver_pool, rng, params)
        }

        TransactionPattern::Consolidation => {
            // Consolidation needs multiple UTXOs
            if utxos_to_spend.len() < 2 {
                return None;
            }
            generate_consolidation_tx(sender, utxos_to_spend, receiver_pool, rng, params)
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

/// Calculate fee for a transaction with given inputs/outputs by building a preliminary unsigned transaction.
/// This is necessary because FeeCalculator needs an actual Transaction to compute the fee.
fn calculate_fee_for_transaction(
    inputs: Vec<TransactionIn>,
    outputs: Vec<TransactionOut>,
    params: &TransactionGenerationParams,
) -> u64 {
    let tx_data = TransactionData {
        version: 0,
        kind: TransactionKind::Payment,
        inputs,
        outputs,
    };

    let prelim_tx = Transaction::new_unsigned(tx_data);
    let calculator = FeeCalculator::new(params.fee_policy.clone());

    calculator.calculate_fee(&prelim_tx)
}

// --- Pattern-Specific Helpers ---

/// Generate a simple 1-input, 1-output (+ optional change) transaction.
fn generate_simple_tx(
    sender: AccountAddress,
    utxo_to_spend: UtxoInfo,
    receiver_pool: &[AccountAddress],
    rng: &mut impl Rng,
    params: &TransactionGenerationParams,
) -> Option<(TransactionData, Vec<TransactionOut>)> {
    let receiver = *receiver_pool.choose(rng)?;
    debug!(choosen_receiver_for_tx = ?receiver.to_string());

    let input = TransactionIn {
        previous_output: utxo_to_spend.outpoint,
        sequence: 0,
    };

    // Calculate base fee with 1 output
    let base_output = TransactionOut {
        value: params.min_output_value,
        recipient: receiver,
    };
    let base_fee = calculate_fee_for_transaction(vec![input.clone()], vec![base_output], params);
    debug!(base_fee_for_tx = ?base_fee);

    // Minimum spendable amount = fee to spend it later + min_output
    // A UTXO is only useful if it can pay for its own spending fee
    let min_spendable = base_fee + params.min_output_value;

    // Check if we have enough
    if utxo_to_spend.value <= min_spendable {
        error!(
            "Don't have enough balance for simple tx of {}. UTXO value: {}, Required: {}",
            sender.to_string(),
            utxo_to_spend.value,
            min_spendable
        );
        return None;
    }

    let available_after_base_fee = utxo_to_spend.value - base_fee;

    // Determine payment value range
    let max_payment = if available_after_base_fee > min_spendable * 2 {
        available_after_base_fee - min_spendable
    } else {
        available_after_base_fee
    };

    let payment_value = rng.random_range(params.min_output_value..=max_payment);

    let mut outputs = vec![TransactionOut {
        value: payment_value,
        recipient: receiver,
    }];

    // Decide on change output
    let potential_change = available_after_base_fee - payment_value;

    // Only create change if it will be economically spendable later
    if potential_change >= min_spendable && params.is_output_worthwhile(potential_change, rng) {
        // Calculate fee with change output
        let with_change = vec![
            TransactionOut {
                value: payment_value,
                recipient: receiver,
            },
            TransactionOut {
                value: potential_change, // temporary value for fee calc
                recipient: sender,
            },
        ];

        let fee_with_change =
            calculate_fee_for_transaction(vec![input.clone()], with_change, params);

        // Check if we can afford the higher fee AND still have min_spendable change
        if utxo_to_spend.value >= payment_value + fee_with_change + min_spendable {
            let change_value = utxo_to_spend.value - payment_value - fee_with_change;

            if change_value >= min_spendable {
                outputs.push(TransactionOut {
                    value: change_value,
                    recipient: sender,
                });
            }
        }
    }

    let tx_data = TransactionData {
        version: 0,
        kind: TransactionKind::Payment,
        inputs: vec![input],
        outputs: outputs.clone(),
    };

    Some((tx_data, outputs))
}

/// Consolidate multiple UTXOs into one (or two with change)
fn generate_consolidation_tx(
    _sender: AccountAddress,
    _utxos_to_spend: Vec<UtxoInfo>,
    _receiver_pool: &[AccountAddress],
    _rng: &mut impl Rng,
    _params: &TransactionGenerationParams,
) -> Option<(TransactionData, Vec<TransactionOut>)> {
    todo!("implement consolidation tx gen")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_balance_per_account() {
        assert_eq!(calculate_balance_per_account(10000, 2), 4950); // rounds to 4950
        assert_eq!(calculate_balance_per_account(100000, 4), 24750); // rounds to 24750
        assert_eq!(calculate_balance_per_account(1234, 2), 610); // rounds to 610
        assert_eq!(calculate_balance_per_account(0, 2), 0);
    }
}
