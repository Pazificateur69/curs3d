#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz JSON deserialization of RPC requests
    let _ = serde_json::from_slice::<curs3d::rpc::RpcRequest>(data);

    // Fuzz RPC envelope parsing
    let _ = serde_json::from_slice::<curs3d::rpc::RpcEnvelope>(data);
});
