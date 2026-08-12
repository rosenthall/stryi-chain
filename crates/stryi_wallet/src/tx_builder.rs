use anyhow::{Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use stryi_core::PrivateKey;
use stryi_core::address::AccountAddress;
use stryi_core::transactions::{
    FeePolicy, OutPoint, Transaction, TransactionData, TransactionIn, TransactionKind,
    TransactionOut,
};

#[derive(Debug, Clone)]
pub struct SpendableUtxo {
    pub outpoint: OutPoint,
    pub value: u64,
}

const DUST_THRESHOLD: u64 = 5_000;

/// Builds and signs a payment transaction using largest-first coin selection from available UTXOs.
pub fn build_payment(
    sender_key: &PrivateKey,
    available_utxos: &[SpendableUtxo],
    recipient: &AccountAddress,
    amount: u64,
    fee_policy: &FeePolicy,
) -> Result<Transaction> {
    if available_utxos.is_empty() {
        bail!("no UTXOs available to spend");
    }

    let mut sorted: Vec<&SpendableUtxo> = available_utxos.iter().collect();
    sorted.sort_by(|a, b| b.value.cmp(&a.value));

    let mut selected: Vec<&SpendableUtxo> = Vec::new();
    let mut total_input: u64 = 0;

    for utxo in &sorted {
        selected.push(utxo);
        total_input += utxo.value;

        let tentative_fee = fee_policy.estimate_fee(selected.len(), 2);
        if total_input >= amount + tentative_fee {
            break;
        }
    }

    let final_fee_1out = fee_policy.estimate_fee(selected.len(), 1);
    let final_fee_2out = fee_policy.estimate_fee(selected.len(), 2);

    if total_input < amount + final_fee_1out {
        bail!(
            "insufficient funds: have {}, need {} (amount) + {} (fee) = {}",
            total_input,
            amount,
            final_fee_1out,
            amount + final_fee_1out,
        );
    }

    let inputs: Vec<TransactionIn> = selected
        .iter()
        .map(|utxo| TransactionIn {
            previous_output: utxo.outpoint,
            sequence: 0,
        })
        .collect();

    let mut outputs = vec![TransactionOut {
        value: amount,
        recipient: *recipient,
    }];

    let remainder = total_input - amount - final_fee_2out;
    if total_input >= amount + final_fee_2out && remainder > DUST_THRESHOLD {
        let sender_address = {
            let signing_key = sender_key.clone().into_inner();
            AccountAddress::from_public_key(signing_key.verifying_key())
        };
        outputs.push(TransactionOut {
            value: remainder,
            recipient: sender_address,
        });
    }

    // verify total inputs cover amount + actual fee
    let actual_fee = fee_policy.estimate_fee(inputs.len(), outputs.len());
    if total_input < amount + actual_fee {
        bail!(
            "insufficient funds after final fee calc: have {}, need {}",
            total_input,
            amount + actual_fee,
        );
    }

    let tx_data = TransactionData {
        version: 0,
        kind: TransactionKind::Payment,
        inputs,
        outputs,
    };

    let signing_key = sender_key.clone().into_inner();
    Ok(tx_data.sign(&signing_key))
}

/// Serializes a signed transaction to base64-encoded postcard for node submission.
pub fn serialize_for_submission(tx: &Transaction) -> Result<String> {
    let bytes =
        postcard::to_stdvec(tx).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(BASE64_STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;
    use rand::rng;
    use stryi_core::PrivateKey;
    use stryi_core::transactions::TransactionHash;

    fn make_key() -> (PrivateKey, AccountAddress) {
        let sk = SigningKey::random(&mut rng());
        let addr = AccountAddress::from_public_key(sk.verifying_key());
        (PrivateKey::new(sk), addr)
    }

    fn make_utxo(value: u64, idx: u8) -> SpendableUtxo {
        SpendableUtxo {
            outpoint: OutPoint {
                txid: TransactionHash::new(&[idx; 32]),
                vout: 0,
            },
            value,
        }
    }

    #[test]
    fn build_simple_payment() {
        let (sender_key, _sender_addr) = make_key();
        let (_recipient_key, recipient_addr) = make_key();
        let policy = FeePolicy::default();

        let utxos = vec![make_utxo(1_000_000, 1)];
        let tx = build_payment(&sender_key, &utxos, &recipient_addr, 100_000, &policy).unwrap();

        assert_eq!(tx.data.kind, TransactionKind::Payment);
        assert_eq!(tx.data.inputs.len(), 1);
        assert_eq!(tx.data.outputs.len(), 2);
        assert_eq!(tx.data.outputs[0].value, 100_000);

        let recovered = tx.recover_public_key().unwrap();
        assert!(tx.verify_signature(&recovered).is_ok());
    }

    #[test]
    fn build_payment_multi_input() {
        let (sender_key, _sender_addr) = make_key();
        let (_recipient_key, recipient_addr) = make_key();
        let policy = FeePolicy::default();

        let utxos = vec![
            make_utxo(50_000, 1),
            make_utxo(60_000, 2),
            make_utxo(70_000, 3),
        ];
        let tx = build_payment(&sender_key, &utxos, &recipient_addr, 100_000, &policy).unwrap();

        assert!(tx.data.inputs.len() >= 2);
        assert_eq!(tx.data.outputs[0].value, 100_000);
    }

    #[test]
    fn build_payment_insufficient_funds() {
        let (sender_key, _) = make_key();
        let (_, recipient_addr) = make_key();
        let policy = FeePolicy::default();

        let utxos = vec![make_utxo(100, 1)];
        let result = build_payment(&sender_key, &utxos, &recipient_addr, 100_000, &policy);
        assert!(result.is_err());
    }

    #[test]
    fn build_payment_no_utxos() {
        let (sender_key, _) = make_key();
        let (_, recipient_addr) = make_key();
        let policy = FeePolicy::default();

        let result = build_payment(&sender_key, &[], &recipient_addr, 100_000, &policy);
        assert!(result.is_err());
    }

    #[test]
    fn fee_estimation_consistency() {
        let policy = FeePolicy::default();
        let fee = policy.estimate_fee(1, 1);
        assert!(fee > 0);
        // more inputs/outputs = higher fee
        assert!(policy.estimate_fee(2, 2) > fee);
    }
}
