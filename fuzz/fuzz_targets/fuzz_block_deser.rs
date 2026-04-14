#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz bincode deserialization of blocks
    let _ = bincode::deserialize::<curs3d::core::block::Block>(data);

    // Fuzz block header deserialization
    let _ = bincode::deserialize::<curs3d::core::block::BlockHeader>(data);
});
