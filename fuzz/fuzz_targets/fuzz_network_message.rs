#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz JSON deserialization of network messages
    let _ = serde_json::from_slice::<curs3d::network::NetworkMessage>(data);

    // Fuzz bincode deserialization of network-level types
    let _ = bincode::deserialize::<curs3d::consensus::EquivocationEvidence>(data);
    let _ = bincode::deserialize::<curs3d::consensus::FinalityVote>(data);
});
