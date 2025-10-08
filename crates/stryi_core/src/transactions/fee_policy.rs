use crate::transactions::Transaction;

/// Represents a static fee calculation policy using fixed costs for transaction components.
///
/// Fee formula:
/// ```text
/// fee = fixed_fee + (input_cost * num_inputs) + (output_cost * num_outputs) + (byte_cost * tx_size)
/// ```
#[derive(Clone, Debug)]
pub struct FeePolicy {
    /// Base fee required for any transaction
    pub fixed_fee: u64,

    /// Cost per transaction input
    pub input_cost: u64,

    /// Cost per transaction output
    pub output_cost: u64,

    /// Cost per byte of serialized(via bincode) transaction
    pub byte_cost: u64,
}

impl Default for FeePolicy {
    /// Creates a default fee policy
    fn default() -> Self {
        Self {
            fixed_fee: 1000,  // 1000 satoshi base fee
            input_cost: 500,  // 500 satoshi per input
            output_cost: 250, // 250 satoshi per output
            byte_cost: 10,    // 10 satoshi per byte
        }
    }
}

impl FeePolicy {
    /// Creates a new fee policy with the specified costs
    pub fn new(fixed_fee: u64, input_cost: u64, output_cost: u64, byte_cost: u64) -> Self {
        Self {
            fixed_fee,
            input_cost,
            output_cost,
            byte_cost,
        }
    }
}

/// Fee calculator that uses FeePolicy to compute transaction fees
pub struct FeeCalculator {
    policy: FeePolicy,
}

impl FeeCalculator {
    /// Creates a new calculator with the specified policy
    pub fn new(policy: FeePolicy) -> Self {
        Self { policy }
    }

    /// Calculates the minimum required fee for a transaction
    pub fn calculate_fee(&self, tx: &Transaction) -> u64 {
        // This avoids doing an actual serialization during fee calculation.
        let tx_bytes = tx.estimate_serialized_size() as u64;

        self.policy.fixed_fee
            + (self.policy.input_cost * tx.data.inputs.len() as u64)
            + (self.policy.output_cost * tx.data.outputs.len() as u64)
            + (self.policy.byte_cost * tx_bytes)
    }

    /// Checks if provided fee is sufficient according to policy
    pub fn is_fee_sufficient(&self, tx: &Transaction, provided_fee: u64) -> bool {
        let required_fee = self.calculate_fee(tx);
        provided_fee >= required_fee
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::AccountAddress;
    use crate::transactions::{
        OutPoint, Transaction, TransactionData, TransactionHash, TransactionIn, TransactionKind,
        TransactionOut,
    };

    fn create_test_transaction(num_inputs: usize, num_outputs: usize) -> Transaction {
        // Create dummy inputs
        let mut inputs = Vec::with_capacity(num_inputs);
        for i in 0..num_inputs {
            inputs.push(TransactionIn {
                previous_output: OutPoint {
                    txid: TransactionHash::new(&[i as u8; 32]),
                    vout: i as u32,
                },
                sequence: 0xFFFFFFFF,
            });
        }

        // Create dummy outputs
        let mut outputs = Vec::with_capacity(num_outputs);
        for i in 0..num_outputs {
            outputs.push(TransactionOut {
                value: 1000,
                recipient: AccountAddress::new(&[i as u8; 20]),
            });
        }

        // Create transaction
        let tx_data = TransactionData {
            version: 1,
            kind: TransactionKind::Payment,
            inputs,
            outputs,
        };

        Transaction {
            data: tx_data,
            signature: Default::default(),
        }
    }

    #[test]
    fn test_fee_calculation() {
        let policy = FeePolicy::default();
        let calculator = FeeCalculator::new(policy.clone());

        // Test case 1: Simple transaction (1 input, 1 output)
        let tx1 = create_test_transaction(1, 1);
        let fee1 = calculator.calculate_fee(&tx1);
        let expected_size1 = tx1.estimate_serialized_size() as u64;

        assert_eq!(
            fee1,
            policy.fixed_fee
                + policy.input_cost
                + policy.output_cost
                + (policy.byte_cost * expected_size1)
        );

        // Test case 2: More complex transaction (3 inputs, 2 outputs)
        let tx2 = create_test_transaction(3, 2);
        let fee2 = calculator.calculate_fee(&tx2);
        let expected_size2 = tx2.estimate_serialized_size() as u64;

        assert_eq!(
            fee2,
            policy.fixed_fee
                + (3 * policy.input_cost)
                + (2 * policy.output_cost)
                + (policy.byte_cost * expected_size2)
        );
    }

    #[test]
    fn test_fee_sufficiency() {
        let calculator = FeeCalculator::new(FeePolicy::default());
        let tx = create_test_transaction(1, 1);
        let required_fee = calculator.calculate_fee(&tx);

        // Test exactly required fee
        assert!(calculator.is_fee_sufficient(&tx, required_fee));

        // Test more than required fee
        assert!(calculator.is_fee_sufficient(&tx, required_fee + 100));

        // Test insufficient fee
        assert!(!calculator.is_fee_sufficient(&tx, required_fee - 1));
        assert!(!calculator.is_fee_sufficient(&tx, 0));
    }
}
