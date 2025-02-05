/// Integration test for common UTXO storage logic,
/// performs some random-values inserts, various batch operations and then checks result
mod utxo_database;

/// Integration test for `get_utxos_for_address`-related logic, and the `addresses` partition by itself
/// Checks correctness for read and checks if `addresses` state correctly updates after delete/delete_batch operations 
mod addresses_integration;
