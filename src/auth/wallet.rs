use ed25519_dalek::{Signer, VerifyingKey};
use std::path::Path;

// Re-export for use in other modules
pub use ed25519_dalek::SigningKey;

/// Load a Solana keypair from a JSON file (Solana CLI format: [u8; 64] array).
/// First 32 bytes are the secret key, last 32 are the public key.
pub fn load_keypair(path: &Path) -> Result<SigningKey, Box<dyn std::error::Error + Send + Sync>> {
    let contents = std::fs::read_to_string(path)?;
    let bytes: Vec<u8> = serde_json::from_str(&contents)?;
    if bytes.len() != 64 {
        return Err(format!("Expected 64-byte keypair, got {} bytes", bytes.len()).into());
    }
    let secret_bytes: [u8; 32] = bytes[..32]
        .try_into()
        .map_err(|_| "Failed to extract secret key")?;
    let signing_key = SigningKey::from_bytes(&secret_bytes);
    Ok(signing_key)
}

/// Get the base58-encoded public key (wallet address).
pub fn wallet_address(keypair: &SigningKey) -> String {
    let pubkey: VerifyingKey = keypair.verifying_key();
    bs58::encode(pubkey.as_bytes()).into_string()
}

/// Sign a message with the keypair, return base58-encoded signature.
pub fn sign_message(keypair: &SigningKey, message: &str) -> String {
    let signature = keypair.sign(message.as_bytes());
    bs58::encode(signature.to_bytes()).into_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_address_roundtrip() {
        let keypair = SigningKey::generate(&mut rand::rngs::OsRng);
        let address = wallet_address(&keypair);
        let sig = sign_message(&keypair, "test message");

        assert!(!address.is_empty());
        assert!(!sig.is_empty());
        // Solana addresses are 32-44 chars base58
        assert!(address.len() >= 32 && address.len() <= 44);
    }

    #[test]
    fn load_keypair_from_json() {
        // Generate a keypair and save as Solana CLI format
        let keypair = SigningKey::generate(&mut rand::rngs::OsRng);
        let pubkey = keypair.verifying_key();
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(keypair.as_bytes());
        bytes.extend_from_slice(pubkey.as_bytes());

        let tmpdir = tempfile::tempdir().unwrap();
        let path = tmpdir.path().join("keypair.json");
        std::fs::write(&path, serde_json::to_string(&bytes).unwrap()).unwrap();

        let loaded = load_keypair(&path).unwrap();
        assert_eq!(wallet_address(&loaded), wallet_address(&keypair));
    }
}
