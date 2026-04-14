#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz bincode deserialization of transactions
    let _ = bincode::deserialize::<curs3d::core::transaction::Transaction>(data);

    // Fuzz JSON deserialization of transactions
    let _ = serde_json::from_slice::<curs3d::core::transaction::Transaction>(data);
});
