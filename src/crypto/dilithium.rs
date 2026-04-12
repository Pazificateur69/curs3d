use pqcrypto_dilithium::dilithium5::{
    detached_sign, keypair, verify_detached_signature, DetachedSignature, PublicKey, SecretKey,
};
use pqcrypto_traits::sign::{DetachedSignature as _, PublicKey as _, SecretKey as _};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyPair {
    pub public_key: Vec<u8>,
    pub secret_key: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signature(pub Vec<u8>);

impl KeyPair {
    pub fn generate() -> Self {
        let (pk, sk) = keypair();
        KeyPair {
            public_key: pk.as_bytes().to_vec(),
            secret_key: sk.as_bytes().to_vec(),
        }
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        let sk = SecretKey::from_bytes(&self.secret_key).expect("invalid secret key");
        let sig = detached_sign(message, &sk);
        Signature(sig.as_bytes().to_vec())
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(&self.public_key)
    }
}

pub fn verify(message: &[u8], signature: &Signature, public_key: &[u8]) -> bool {
    let pk = match PublicKey::from_bytes(public_key) {
        Ok(pk) => pk,
        Err(_) => return false,
    };
    let sig = match DetachedSignature::from_bytes(&signature.0) {
        Ok(sig) => sig,
        Err(_) => return false,
    };
    verify_detached_signature(&sig, message, &pk).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_and_verify() {
        let kp = KeyPair::generate();
        let msg = b"CURS3D quantum-resistant blockchain";
        let sig = kp.sign(msg);
        assert!(verify(msg, &sig, &kp.public_key));
    }

    #[test]
    fn test_invalid_signature() {
        let kp = KeyPair::generate();
        let kp2 = KeyPair::generate();
        let msg = b"test message";
        let sig = kp.sign(msg);
        assert!(!verify(msg, &sig, &kp2.public_key));
    }
}
