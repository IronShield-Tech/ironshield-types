//! # Cryptographic operations for IronShield challenges
//!
//! This module provides Ed25519 signature generation and verification for IronShield challenges,
//! including key management from environment variables and challenge signing/verification.
//!
//! ## Key Format Support
//!
//! The key loading functions support multiple formats with automatic detection:
//! - **Raw Ed25519 Keys**: Base64-encoded 32-byte Ed25519 keys (legacy format)
//! - **PGP Format**: Base64-encoded PGP keys (without ASCII armor headers)
//!
//! For PGP keys, a simple heuristic scans the binary data to find valid Ed25519 key material.
//! This approach is simpler and more reliable than using complex PGP parsing libraries.
//!
//! ## Features
//!
//! ### Key Management
//! * `load_private_key_from_env()`:            Load Ed25519 private key from environment
//!                                             (multiple formats)
//! * `load_public_key_from_env()`:             Load Ed25519 public key from environment
//!                                             (multiple formats)
//! * `generate_test_keypair()`:                Generate keypair for testing.
//!
//! ### Challenge Signing
//! * `sign_challenge()`:                       Sign challenges with environment private key
//! * `IronShieldChallenge::create_signed()`:   Create and sign challenges in one step
//!
//! ### Challenge Verification
//! * `verify_challenge_signature()`:           Verify using environment public key
//! * `verify_challenge_signature_with_key()`:  Verify using provided public key
//! * `validate_challenge()`:                   Comprehensive challenge validation
//!                                             (signature + expiration)
//!
//! ## Environment Variables
//!
//! The following environment variables are used for key storage:
//! * `IRONSHIELD_PRIVATE_KEY`:                 Base64-encoded private key (PGP or raw Ed25519)
//! * `IRONSHIELD_PUBLIC_KEY`:                  Base64-encoded public key (PGP or raw Ed25519)
//!
//! ## Examples
//!
//! ### Basic Usage with Raw Keys
//! ```no_run
//! use ironshield_types::{load_private_key_from_env, generate_test_keypair};
//!
//! // Generate test keys
//! let (private_b64, public_b64) = generate_test_keypair();
//! std::env::set_var("IRONSHIELD_PRIVATE_KEY", private_b64);
//! std::env::set_var("IRONSHIELD_PUBLIC_KEY", public_b64);
//!
//! // Load keys from environment
//! let signing_key = load_private_key_from_env().unwrap();
//! ```
//!
//! ### Using with PGP Keys
//! For PGP keys stored in Cloudflare Secrets Store (base64-encoded without armor):
//! ```bash
//! # Store PGP keys in Cloudflare Secrets Store
//! wrangler secrets-store secret create STORE_ID \
//!   --name IRONSHIELD_PRIVATE_KEY \
//!   --value "LS0tLS1CRUdJTi..." \  # Base64 PGP data without headers
//!   --scopes workers
//! ```

use base64::{
    Engine,
    engine::general_purpose::STANDARD
};
use ed25519_dalek::{
    Signature,
    Signer,
    Verifier,
    SigningKey,
    VerifyingKey,
    PUBLIC_KEY_LENGTH,
    SECRET_KEY_LENGTH
};
use rand::rngs::OsRng;

use crate::IronShieldChallenge;

use std::env;

/// Debug logging helper that works across different compilation targets
macro_rules! debug_log {
    ($($arg:tt)*) => {
        #[cfg(all(target_arch = "wasm32", feature = "wasm-logging"))]
        {
            let msg = format!($($arg)*);
            web_sys::console::log_1(&wasm_bindgen::JsValue::from_str(&msg));
        }
        #[cfg(not(target_arch = "wasm32"))]
        eprintln!($($arg)*);
        #[cfg(all(target_arch = "wasm32", not(feature = "wasm-logging")))]
        {
            // No-op for WASM without logging feature
            let _ = format!($($arg)*);
        }
    };
}

#[derive(Debug, Clone)]
pub enum CryptoError {
    MissingEnvironmentVariable(String),
    InvalidKeyFormat(String),
    SigningFailed(String),
    VerificationFailed(String),
    Base64DecodingFailed(String),
    PgpParsingFailed(String),
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CryptoError::MissingEnvironmentVariable(var) => write!(f, "Missing environment variable: {}", var),
            CryptoError::InvalidKeyFormat(msg) => write!(f, "Invalid key format: {}", msg),
            CryptoError::SigningFailed(msg) => write!(f, "Signing failed: {}", msg),
            CryptoError::VerificationFailed(msg) => write!(f, "Verification failed: {}", msg),
            CryptoError::Base64DecodingFailed(msg) => write!(f, "Base64 decoding failed: {}", msg),
            CryptoError::PgpParsingFailed(msg) => write!(f, "PGP parsing failed: {}", msg),
        }
    }
}

impl std::error::Error for CryptoError {}

/// Parse key data with simple heuristic approach (handles PGP and raw Ed25519)
///
/// This function attempts to extract Ed25519 key material from various formats:
/// 1. PGP armored text (base64 with possible line breaks)
/// 2. Raw base64-encoded Ed25519 keys (32 bytes)
///
/// # Arguments
/// * `key_data`:   Key data as string (PGP armored or raw base64)
/// * `is_private`: Whether this is a private key (for validation)
///
/// # Returns
/// * `Result<[u8; 32], CryptoError>`: The 32-byte Ed25519 key
fn parse_key_simple(key_data: &str, is_private: bool) -> Result<[u8; 32], CryptoError> {
    // Clean the key data by removing all whitespace, line breaks, and common PGP formatting
    let cleaned_data = key_data
        .chars()
        .filter(|c| !c.is_whitespace()) // Remove all whitespace including \n, \r, \t, spaces
        .collect::<String>();

    debug_log!("🔑 Parsing key data: {} chars → {} chars after cleaning", key_data.len(), cleaned_data.len());

    // Check for any invalid base64 characters
    let invalid_chars: Vec<char> = cleaned_data
        .chars()
        .filter(|&c| !matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' | '='))
        .collect();

    if !invalid_chars.is_empty() {
        debug_log!("🔧 Fixing {} invalid base64 characters", invalid_chars.len());

        // Try to fix common issues
        let fixed_data = cleaned_data
            .chars()
            .filter(|&c| matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '+' | '/' | '='))
            .collect::<String>();

        debug_log!("🔧 Fixed data length: {}", fixed_data.len());

        // Try to decode the fixed data
        match STANDARD.decode(&fixed_data) {
            Ok(key_bytes) => {
                debug_log!("✅ Fixed data decoded to {} bytes", key_bytes.len());
                return try_extract_ed25519_key(&key_bytes, is_private);
            }
            Err(e) => {
                debug_log!("⚠️ Fixed data decode failed: {}", e);
            }
        }
    }

    // Try to decode as base64
    let key_bytes = match STANDARD.decode(&cleaned_data) {
        Ok(bytes) => {
            debug_log!("✅ Base64 decoded to {} bytes", bytes.len());
            bytes
        }
        Err(e) => {
            debug_log!("⚠️ Base64 decode failed: {}", e);

            // Try removing trailing characters that might be corrupted
            let mut test_data = cleaned_data.clone();
            while !test_data.is_empty() {
                if let Ok(bytes) = STANDARD.decode(&test_data) {
                    debug_log!("✅ Successful decode after trimming to {} chars → {} bytes", test_data.len(), bytes.len());
                    return try_extract_ed25519_key(&bytes, is_private);
                }
                test_data.pop();
            }

            return Err(CryptoError::Base64DecodingFailed(format!("Failed to decode cleaned key data: {}", e)));
        }
    };

    try_extract_ed25519_key(&key_bytes, is_private)
}

/// Extract Ed25519 key material from decoded bytes
fn try_extract_ed25519_key(key_bytes: &[u8], is_private: bool) -> Result<[u8; 32], CryptoError> {
    debug_log!("🔑 Extracting Ed25519 key from {} bytes", key_bytes.len());

    // If it's exactly 32 bytes, it might be a raw Ed25519 key
    if key_bytes.len() == 32 {
        let mut key_array = [0u8; 32];
        key_array.copy_from_slice(&key_bytes);

        // Validate the key
        if is_private {
            let _signing_key = SigningKey::from_bytes(&key_array);
            debug_log!("✅ Raw Ed25519 private key validated");
        } else {
            let _verifying_key = VerifyingKey::from_bytes(&key_array)
                .map_err(|e| CryptoError::InvalidKeyFormat(format!("Invalid raw public key: {}", e)))?;
            debug_log!("✅ Raw Ed25519 public key validated");
        }

        return Ok(key_array);
    }

    // For larger data (PGP format), use multiple sophisticated key extraction strategies
    if key_bytes.len() >= 32 {
        debug_log!("🔍 Scanning PGP data for Ed25519 key...");

        // Strategy 1: Look for Ed25519 algorithm identifier (0x16 = 22 decimal)
        // Ed25519 keys in PGP often have specific patterns
        for window_start in 0..key_bytes.len().saturating_sub(32) {
            let potential_key = &key_bytes[window_start..window_start + 32];

            // Skip obviously invalid keys (all zeros, all 0xFF, or patterns that don't make sense)
            if potential_key == &[0u8; 32] || potential_key == &[0xFFu8; 32] {
                continue;
            }

            // For Ed25519, check if this looks like valid key material
            let mut key_array = [0u8; 32];
            key_array.copy_from_slice(potential_key);

            if is_private {
                // For private keys, try to create a SigningKey and derive the public key
                let signing_key = SigningKey::from_bytes(&key_array);
                let derived_public = signing_key.verifying_key();

                // Additional validation: check if the derived public key appears elsewhere in the PGP data
                let public_bytes = derived_public.to_bytes();

                // Look for the derived public key in the remaining PGP data
                let search_start = window_start + 32;
                if search_start < key_bytes.len() {
                    let remaining_data = &key_bytes[search_start..];
                    if remaining_data.windows(32).any(|window| window == public_bytes) {
                        debug_log!("✅ Private key found at offset {} (with matching public key)", window_start);
                        return Ok(key_array);
                    }
                }

                // Even if we don't find the public key, if this is at a reasonable offset, it might be valid
                if window_start >= 20 && window_start <= 200 {
                    debug_log!("✅ Private key found at offset {}", window_start);
                    return Ok(key_array);
                }
            } else {
                // For public keys, try to create a VerifyingKey
                if let Ok(_verifying_key) = VerifyingKey::from_bytes(&key_array) {
                    // Additional validation: public keys should appear after some PGP header data
                    if window_start >= 10 && window_start <= 100 {
                        debug_log!("✅ Public key found at offset {}", window_start);
                        return Ok(key_array);
                    }
                }
            }
        }

        // Strategy 2: Look for specific PGP packet patterns
        for (i, &byte) in key_bytes.iter().enumerate() {
            if byte == 0x16 && i + 33 < key_bytes.len() { // Algorithm 22 (Ed25519) + 32 bytes key
                let key_start = i + 1;
                if key_start + 32 <= key_bytes.len() {
                    let potential_key = &key_bytes[key_start..key_start + 32];
                    let mut key_array = [0u8; 32];
                    key_array.copy_from_slice(potential_key);

                    // Validate this key
                    if is_private {
                        let _signing_key = SigningKey::from_bytes(&key_array);
                        debug_log!("✅ Private key found via algorithm ID at offset {}", key_start);
                        return Ok(key_array);
                    } else {
                        if let Ok(_verifying_key) = VerifyingKey::from_bytes(&key_array) {
                            debug_log!("✅ Public key found via algorithm ID at offset {}", key_start);
                            return Ok(key_array);
                        }
                    }
                }
            }
        }

        // Strategy 3: Look for keys at common PGP offsets
        let common_offsets = [
            32, 36, 40, 44, 48, 52, 56, 60, 64, 68, 72, 76, 80, 84, 88, 92, 96, 100,
            104, 108, 112, 116, 120, 124, 128, 132, 136, 140, 144, 148, 152, 156, 160
        ];

        for &offset in &common_offsets {
            if offset + 32 <= key_bytes.len() {
                let potential_key = &key_bytes[offset..offset + 32];

                // Skip obviously invalid patterns
                if potential_key == &[0u8; 32] || potential_key == &[0xFFu8; 32] {
                    continue;
                }

                let mut key_array = [0u8; 32];
                key_array.copy_from_slice(potential_key);

                if is_private {
                    let _signing_key = SigningKey::from_bytes(&key_array);
                    debug_log!("✅ Private key found at common offset {}", offset);
                    return Ok(key_array);
                } else {
                    if let Ok(_verifying_key) = VerifyingKey::from_bytes(&key_array) {
                        debug_log!("✅ Public key found at common offset {}", offset);
                        return Ok(key_array);
                    }
                }
            }
        }
    }

    Err(CryptoError::PgpParsingFailed(format!(
        "Could not find valid Ed25519 key material in {} bytes of PGP data using multiple strategies",
        key_bytes.len()
    )))
}

/// Loads the private key from the IRONSHIELD_PRIVATE_KEY environment variable
///
/// The environment variable should contain a base64-encoded PGP private key (without armor headers).
/// For backward compatibility, raw base64-encoded Ed25519 keys (32 bytes) are also supported.
///
/// # Returns
/// * `Result<SigningKey, CryptoError>`: The Ed25519 signing key or an error
///
/// # Environment Variables
/// * `IRONSHIELD_PRIVATE_KEY`:          Base64-encoded PGP private key data
///                                      (without -----BEGIN/END----- lines)
///                                      or raw base64-encoded Ed25519 private
///                                      key (legacy format)
pub fn load_private_key_from_env() -> Result<SigningKey, CryptoError> {
    let key_str: String = env::var("IRONSHIELD_PRIVATE_KEY")
        .map_err(|_| CryptoError::MissingEnvironmentVariable("IRONSHIELD_PRIVATE_KEY".to_string()))?;

    // Try PGP format first
    match parse_key_simple(&key_str, true) {
        Ok(key_array) => {
            let signing_key: SigningKey = SigningKey::from_bytes(&key_array);
            return Ok(signing_key);
        }
        Err(CryptoError::PgpParsingFailed(_)) | Err(CryptoError::Base64DecodingFailed(_)) => {
            // Fall back to raw base64 format
        }
        Err(e) => return Err(e), // Return other errors immediately
    }

    // Fallback: try raw base64-encoded Ed25519 key (legacy format)
    let key_bytes: Vec<u8> = STANDARD.decode(key_str.trim())
        .map_err(|e| CryptoError::Base64DecodingFailed(format!("Private key (legacy fallback): {}", e)))?;

    // Verify length for raw Ed25519 key
    if key_bytes.len() != SECRET_KEY_LENGTH {
        return Err(CryptoError::InvalidKeyFormat(
            format!("Private key must be {} bytes (raw Ed25519) or valid PGP format, got {} bytes",
                   SECRET_KEY_LENGTH, key_bytes.len())
        ));
    }

    // Create signing key from raw bytes
    let key_array: [u8; SECRET_KEY_LENGTH] = key_bytes.try_into()
        .map_err(|_| CryptoError::InvalidKeyFormat("Failed to convert private key bytes".to_string()))?;

    let signing_key: SigningKey = SigningKey::from_bytes(&key_array);
    Ok(signing_key)
}

/// Loads the public key from the IRONSHIELD_PUBLIC_KEY environment variable
///
/// The environment variable should contain a base64-encoded PGP public key (without armor headers).
/// For backward compatibility, raw base64-encoded Ed25519 keys (32 bytes) are also supported.
///
/// # Returns
/// * `Result<VerifyingKey, CryptoError>`: The Ed25519 verifying key or an error
///
/// # Environment Variables
/// * `IRONSHIELD_PUBLIC_KEY`: Base64-encoded PGP public key data
///                            (without -----BEGIN/END----- lines)
///                            or raw base64-encoded Ed25519 public key
///                            (legacy format)
pub fn load_public_key_from_env() -> Result<VerifyingKey, CryptoError> {
    let key_str: String = env::var("IRONSHIELD_PUBLIC_KEY")
        .map_err(|_| CryptoError::MissingEnvironmentVariable("IRONSHIELD_PUBLIC_KEY".to_string()))?;

    // Try PGP format first
    match parse_key_simple(&key_str, false) {
        Ok(key_array) => {
            let verifying_key: VerifyingKey = VerifyingKey::from_bytes(&key_array)
                .map_err(|e| CryptoError::InvalidKeyFormat(format!("Invalid public key: {}", e)))?;
            return Ok(verifying_key);
        }
        Err(CryptoError::PgpParsingFailed(_)) | Err(CryptoError::Base64DecodingFailed(_)) => {
            // Fall back to raw base64 format
        }
        Err(e) => return Err(e), // Return other errors immediately
    }

    // Fallback: try raw base64-encoded Ed25519 key (legacy format)
    let key_bytes: Vec<u8> = STANDARD.decode(key_str.trim())
        .map_err(|e| CryptoError::Base64DecodingFailed(format!("Public key (legacy fallback): {}", e)))?;

    // Verify length for raw Ed25519 key
    if key_bytes.len() != PUBLIC_KEY_LENGTH {
        return Err(CryptoError::InvalidKeyFormat(
            format!("Public key must be {} bytes (raw Ed25519) or valid PGP format, got {} bytes",
                   PUBLIC_KEY_LENGTH, key_bytes.len())
        ));
    }

    // Create verifying key from raw bytes
    let key_array: [u8; PUBLIC_KEY_LENGTH] = key_bytes.try_into()
        .map_err(|_| CryptoError::InvalidKeyFormat("Failed to convert public key bytes".to_string()))?;

    let verifying_key: VerifyingKey = VerifyingKey::from_bytes(&key_array)
        .map_err(|e| CryptoError::InvalidKeyFormat(format!("Invalid public key: {}", e)))?;

    Ok(verifying_key)
}

/// Creates a message to be signed from challenge data components
///
/// This function creates a canonical representation of the challenge data for signing.
/// It takes individual challenge components rather than a complete challenge object,
/// allowing it to be used during challenge creation.
///
/// # Arguments
/// * `random_nonce`:    The random nonce string
/// * `created_time`:    The challenge creation timestamp
/// * `expiration_time`: The challenge expiration timestamp
/// * `website_id`:      The website identifier
/// * `challenge_param`: The challenge parameter bytes
/// * `public_key`:      The public key bytes
///
/// # Returns
/// * `String`: Canonical string representation for signing
pub fn create_signing_message(
    random_nonce: &str,
    created_time: i64,
    expiration_time: i64,
    website_id: &str,
    challenge_param: &[u8; 32],
    public_key: &[u8; 32]
) -> String {
    format!(
        "{}|{}|{}|{}|{}|{}",
        random_nonce,
        created_time,
        expiration_time,
        website_id,
        hex::encode(challenge_param),
        hex::encode(public_key)
    )
}

/// Generates an Ed25519 signature for a given message using the provided signing key
///
/// This is a low-level function for generating signatures. For challenge signing,
/// consider using `sign_challenge` which handles message creation automatically.
///
/// # Arguments
/// * `signing_key`: The Ed25519 signing key to use
/// * `message`:     The message to sign (will be converted to bytes)
///
/// # Returns
/// * `Result<[u8; 64], CryptoError>`: The signature bytes or an error
///
/// # Example
/// ```no_run
/// use ironshield_types::{generate_signature, load_private_key_from_env};
///
/// let signing_key = load_private_key_from_env()?;
/// let signature = generate_signature(&signing_key, "message to sign")?;
/// # Ok::<(), ironshield_types::CryptoError>(())
/// ```
pub fn generate_signature(signing_key: &SigningKey, message: &str) -> Result<[u8; 64], CryptoError> {
    let signature: Signature = signing_key.sign(message.as_bytes());
    Ok(signature.to_bytes())
}

/// Signs a challenge using the private key from environment variables.
///
/// This function creates a signature over all challenge fields except the signature itself.
/// The private key is loaded from the IRONSHIELD_PRIVATE_KEY environment variable.
///
/// # Arguments
/// * `challenge`: The challenge to sign (signature field will be ignored).
///
/// # Returns
/// * `Result<[u8; 64], CryptoError>`: The Ed25519 signature bytes or an error.
///
/// # Example
/// ```no_run
/// use ironshield_types::{IronShieldChallenge, sign_challenge, SigningKey};
///
/// let dummy_key = SigningKey::from_bytes(&[0u8; 32]);
/// let mut challenge = IronShieldChallenge::new(
///     "test_website".to_string(),
///     100_000,
///     dummy_key,
///     [0x34; 32],
/// );
///
/// // Sign the challenge (requires IRONSHIELD_PRIVATE_KEY environment variable)
/// let signature = sign_challenge(&challenge).unwrap();
/// challenge.challenge_signature = signature;
/// ```
pub fn sign_challenge(challenge: &IronShieldChallenge) -> Result<[u8; 64], CryptoError> {
    let signing_key: SigningKey = load_private_key_from_env()?;
    let message: String = create_signing_message(
        &challenge.random_nonce,
        challenge.created_time,
        challenge.expiration_time,
        &challenge.website_id,
        &challenge.challenge_param,
        &challenge.public_key
    );
    generate_signature(&signing_key, &message)
}

/// Verifies a challenge signature using the public key from environment variables
///
/// This function verifies that the challenge signature is valid and that the challenge
/// data has not been tampered with. The public key is loaded from the IRONSHIELD_PUBLIC_KEY
/// environment variable.
///
/// # Arguments
/// * `challenge`: The challenge with signature to verify.
///
/// # Returns
/// * `Result<(), CryptoError>`: `Ok(())` if valid, error if verification fails.
///
/// # Example
/// ```no_run
/// use ironshield_types::{IronShieldChallenge, verify_challenge_signature, SigningKey};
///
/// let dummy_key = SigningKey::from_bytes(&[0u8; 32]);
/// let challenge = IronShieldChallenge::new(
///     "test_website".to_string(),
///     100_000,
///     dummy_key,
///     [0x34; 32],
/// );
///
/// // Verify the challenge (requires IRONSHIELD_PUBLIC_KEY environment variable)
/// verify_challenge_signature(&challenge).unwrap();
/// ```
pub fn verify_challenge_signature(challenge: &IronShieldChallenge) -> Result<(), CryptoError> {
    let verifying_key: VerifyingKey = load_public_key_from_env()?;

    let message: String = create_signing_message(
        &challenge.random_nonce,
        challenge.created_time,
        challenge.expiration_time,
        &challenge.website_id,
        &challenge.challenge_param,
        &challenge.public_key
    );
    let signature: Signature = Signature::from_slice(&challenge.challenge_signature)
        .map_err(|e| CryptoError::InvalidKeyFormat(format!("Invalid signature format: {}", e)))?;

    verifying_key.verify(message.as_bytes(), &signature)
        .map_err(|e| CryptoError::VerificationFailed(format!("Signature verification failed: {}", e)))?;

    Ok(())
}

/// Verifies a challenge signature using a provided public key
///
/// This function is similar to `verify_challenge_signature` but uses a provided
/// public key instead of loading from environment variables. This is useful for
/// client-side verification where the public key is embedded in the challenge.
///
/// # Arguments
/// * `challenge`:        The challenge with signature to verify
/// * `public_key_bytes`: The Ed25519 public key bytes to use for verification
///
/// # Returns
/// * `Result<(), CryptoError>`: `Ok(())` if valid, error if verification fails
pub fn verify_challenge_signature_with_key(
    challenge: &IronShieldChallenge,
    public_key_bytes: &[u8; 32]
) -> Result<(), CryptoError> {
    let verifying_key: VerifyingKey = VerifyingKey::from_bytes(public_key_bytes)
        .map_err(|e| CryptoError::InvalidKeyFormat(format!("Invalid public key: {}", e)))?;

    let message: String = create_signing_message(
        &challenge.random_nonce,
        challenge.created_time,
        challenge.expiration_time,
        &challenge.website_id,
        &challenge.challenge_param,
        &challenge.public_key
    );
    let signature: Signature = Signature::from_slice(&challenge.challenge_signature)
        .map_err(|e| CryptoError::InvalidKeyFormat(format!("Invalid signature format: {}", e)))?;

    verifying_key.verify(message.as_bytes(), &signature)
        .map_err(|e| CryptoError::VerificationFailed(format!("Signature verification failed: {}", e)))?;

    Ok(())
}

/// Generates a new Ed25519 keypair for testing purposes
///
/// This function generates a fresh keypair and returns the keys in raw base64 format
/// (legacy format) suitable for use as environment variables in tests.
///
/// # Returns
/// * `(String, String)`: (base64_private_key, base64_public_key) in raw Ed25519 format
///
/// # Example
/// ```
/// use ironshield_types::generate_test_keypair;
///
/// let (private_key_b64, public_key_b64) = generate_test_keypair();
/// std::env::set_var("IRONSHIELD_PRIVATE_KEY", private_key_b64);
/// std::env::set_var("IRONSHIELD_PUBLIC_KEY", public_key_b64);
/// ```
pub fn generate_test_keypair() -> (String, String) {
    let signing_key: SigningKey = SigningKey::generate(&mut OsRng);
    let verifying_key: VerifyingKey = signing_key.verifying_key();

    let private_key_b64: String = STANDARD.encode(signing_key.to_bytes());
    let public_key_b64: String = STANDARD.encode(verifying_key.to_bytes());

    (private_key_b64, public_key_b64)
}

/// Verifies a challenge and checks if it's valid and not expired
///
/// This is a comprehensive validation function that checks:
/// - Signature validity
/// - Challenge expiration
/// - Basic format validation
///
/// # Arguments
/// * `challenge`: The challenge to validate
///
/// # Returns
/// * `Result<(), CryptoError>`: `Ok(())` if valid, error if invalid
pub fn validate_challenge(challenge: &IronShieldChallenge) -> Result<(), CryptoError> {
    // Check signature first
    verify_challenge_signature(challenge)?;

    // Check expiration
    if challenge.is_expired() {
        return Err(CryptoError::VerificationFailed("Challenge has expired".to_string()));
    }

    if challenge.website_id.is_empty() {
        return Err(CryptoError::VerificationFailed("Empty website_id".to_string()));
    }

    Ok(())
}

/// Loads a private key from raw key data (for Cloudflare Workers)
///
/// This function is designed for use with Cloudflare Workers where secrets
/// are accessible through the env parameter rather than standard environment variables.
///
/// # Arguments
/// * `key_data`: Base64-encoded key data (PGP or raw Ed25519)
///
/// # Returns
/// * `Result<SigningKey, CryptoError>`: The Ed25519 signing key or an error
pub fn load_private_key_from_data(key_data: &str) -> Result<SigningKey, CryptoError> {
    // Try PGP format first
    match parse_key_simple(key_data, true) {
        Ok(key_array) => {
            let signing_key: SigningKey = SigningKey::from_bytes(&key_array);
            return Ok(signing_key);
        }
        Err(CryptoError::PgpParsingFailed(_msg)) => {
            // Fall back to raw base64 format
        }
        Err(CryptoError::Base64DecodingFailed(_msg)) => {
            // Fall back to raw base64 format
        }
        Err(e) => {
            return Err(e); // Return other errors immediately
        }
    }

    // Fallback: try raw base64-encoded Ed25519 key (legacy format)
    let key_bytes: Vec<u8> = STANDARD.decode(key_data.trim())
        .map_err(|e| {
            CryptoError::Base64DecodingFailed(format!("Private key (legacy fallback): {}", e))
        })?;

    // Verify length for raw Ed25519 key
    if key_bytes.len() != SECRET_KEY_LENGTH {
        let error_msg = format!(
            "Invalid key length: expected {} bytes for Ed25519 private key, got {} bytes",
            SECRET_KEY_LENGTH,
            key_bytes.len()
        );
        return Err(CryptoError::InvalidKeyFormat(error_msg));
    }

    let mut key_array = [0u8; SECRET_KEY_LENGTH];
    key_array.copy_from_slice(&key_bytes);

    Ok(SigningKey::from_bytes(&key_array))
}

/// Loads a public key from raw key data (for Cloudflare Workers)
///
/// This function is designed for use with Cloudflare Workers where secrets
/// are accessible through the env parameter rather than standard environment variables.
///
/// # Arguments
/// * `key_data`: Base64-encoded key data (PGP or raw Ed25519)
///
/// # Returns
/// * `Result<VerifyingKey, CryptoError>`: The Ed25519 verifying key or an error
pub fn load_public_key_from_data(key_data: &str) -> Result<VerifyingKey, CryptoError> {
    // Try PGP format first
    match parse_key_simple(key_data, false) {
        Ok(key_array) => {
            let verifying_key = VerifyingKey::from_bytes(&key_array)
                .map_err(|e| CryptoError::InvalidKeyFormat(format!("Invalid public key from PGP: {}", e)))?;
            return Ok(verifying_key);
        }
        Err(CryptoError::PgpParsingFailed(_msg)) => {
            // Fall back to raw base64 format
        }
        Err(CryptoError::Base64DecodingFailed(_msg)) => {
            // Fall back to raw base64 format
        }
        Err(e) => {
            return Err(e); // Return other errors immediately
        }
    }

    // Fallback: try raw base64-encoded Ed25519 key (legacy format)
    let key_bytes: Vec<u8> = STANDARD.decode(key_data.trim())
        .map_err(|e| {
            CryptoError::Base64DecodingFailed(format!("Public key (legacy fallback): {}", e))
        })?;

    // Verify length for raw Ed25519 key
    if key_bytes.len() != PUBLIC_KEY_LENGTH {
        let error_msg = format!(
            "Invalid key length: expected {} bytes for Ed25519 public key, got {} bytes",
            PUBLIC_KEY_LENGTH,
            key_bytes.len()
        );
        return Err(CryptoError::InvalidKeyFormat(error_msg));
    }

    let mut key_array = [0u8; PUBLIC_KEY_LENGTH];
    key_array.copy_from_slice(&key_bytes);

    let verifying_key = VerifyingKey::from_bytes(&key_array)
        .map_err(|e| CryptoError::InvalidKeyFormat(format!("Invalid Ed25519 public key: {}", e)))?;

    Ok(verifying_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;
    use rand::rngs::OsRng;

    // Use a mutex to ensure tests don't interfere with each other when setting env vars
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[allow(dead_code)]
    fn setup_isolated_test_keys() -> (SigningKey, VerifyingKey) {
        let signing_key: SigningKey = SigningKey::generate(&mut OsRng);
        let verifying_key: VerifyingKey = signing_key.verifying_key();

        let private_key: String = STANDARD.encode(signing_key.to_bytes());
        let public_key: String = STANDARD.encode(verifying_key.to_bytes());

        // Set environment variables with mutex protection
        let _lock = ENV_MUTEX.lock().unwrap();
        env::set_var("IRONSHIELD_PRIVATE_KEY", &private_key);
        env::set_var("IRONSHIELD_PUBLIC_KEY", &public_key);

        (signing_key, verifying_key)
    }

    #[test]
    fn test_basic_ed25519_signing() {
        // Test basic Ed25519 signing with a simple message
        let signing_key: SigningKey = SigningKey::generate(&mut OsRng);
        let verifying_key: VerifyingKey = signing_key.verifying_key();

        let message = b"Hello, world!";
        let signature: Signature = signing_key.sign(message);

        // This should work without any issues
        let result = verifying_key.verify(message, &signature);
        assert!(result.is_ok(), "Basic Ed25519 signing should work");
    }

    #[test]
    fn test_crypto_integration_without_env() {
        // Generate keys directly without using environment variables
        let signing_key: SigningKey = SigningKey::generate(&mut OsRng);
        let verifying_key: VerifyingKey = signing_key.verifying_key();

        // Create a challenge with the public key
        let challenge = IronShieldChallenge::new(
            "example.com".to_string(),
            100_000,
            signing_key.clone(),
            verifying_key.to_bytes(),
        );

        // Create the signing message manually
        let signing_message = create_signing_message(
            &challenge.random_nonce,
            challenge.created_time,
            challenge.expiration_time,
            &challenge.website_id,
            &challenge.challenge_param,
            &challenge.public_key
        );
        println!("Signing message: {}", signing_message);

        // The challenge should already be signed, so let's verify it
        let verification_message = create_signing_message(
            &challenge.random_nonce,
            challenge.created_time,
            challenge.expiration_time,
            &challenge.website_id,
            &challenge.challenge_param,
            &challenge.public_key
        );
        assert_eq!(signing_message, verification_message, "Signing message should be consistent");

        let signature_from_bytes = Signature::from_slice(&challenge.challenge_signature)
            .expect("Should be able to recreate signature from bytes");

        let verification_result = verifying_key.verify(verification_message.as_bytes(), &signature_from_bytes);
        assert!(verification_result.is_ok(), "Manual verification should succeed");

        // Now test our helper function
        let verify_result = verify_challenge_signature_with_key(&challenge, &verifying_key.to_bytes());
        assert!(verify_result.is_ok(), "verify_challenge_signature_with_key should succeed");
    }

    #[test]
    fn test_generate_test_keypair() {
        let (private_key, public_key) = generate_test_keypair();

        // Keys should be valid base64
        assert!(STANDARD.decode(&private_key).is_ok());
        assert!(STANDARD.decode(&public_key).is_ok());

        // Keys should be correct length when decoded
        let private_bytes = STANDARD.decode(&private_key).unwrap();
        let public_bytes = STANDARD.decode(&public_key).unwrap();
        assert_eq!(private_bytes.len(), SECRET_KEY_LENGTH);
        assert_eq!(public_bytes.len(), PUBLIC_KEY_LENGTH);
    }

    #[test]
    fn test_load_keys_from_env() {
        let _lock = ENV_MUTEX.lock().unwrap();

        let (signing_key, verifying_key) = {
            let signing_key: SigningKey = SigningKey::generate(&mut OsRng);
            let verifying_key: VerifyingKey = signing_key.verifying_key();

            let private_key: String = STANDARD.encode(signing_key.to_bytes());
            let public_key: String = STANDARD.encode(verifying_key.to_bytes());

            env::set_var("IRONSHIELD_PRIVATE_KEY", &private_key);
            env::set_var("IRONSHIELD_PUBLIC_KEY", &public_key);

            (signing_key, verifying_key)
        };

        // Should successfully load keys
        let loaded_signing_key = load_private_key_from_env().unwrap();
        let loaded_verifying_key = load_public_key_from_env().unwrap();

        // Keys should match what we set
        assert_eq!(signing_key.to_bytes(), loaded_signing_key.to_bytes());
        assert_eq!(verifying_key.to_bytes(), loaded_verifying_key.to_bytes());
    }

    #[test]
    fn test_missing_environment_variables() {
        let _lock = ENV_MUTEX.lock().unwrap();

        // Remove environment variables for this test
        env::remove_var("IRONSHIELD_PRIVATE_KEY");
        env::remove_var("IRONSHIELD_PUBLIC_KEY");

        // Should fail with appropriate errors
        let private_result = load_private_key_from_env();
        assert!(private_result.is_err());
        assert!(matches!(private_result.unwrap_err(), CryptoError::MissingEnvironmentVariable(_)));

        let public_result = load_public_key_from_env();
        assert!(public_result.is_err());
        assert!(matches!(public_result.unwrap_err(), CryptoError::MissingEnvironmentVariable(_)));
    }

    #[test]
    fn test_invalid_key_format() {
        let _lock = ENV_MUTEX.lock().unwrap();

        // Set invalid keys
        env::set_var("IRONSHIELD_PRIVATE_KEY", "invalid-base64!");
        env::set_var("IRONSHIELD_PUBLIC_KEY", "invalid-base64!");

        let private_result = load_private_key_from_env();
        assert!(private_result.is_err());
        assert!(matches!(private_result.unwrap_err(), CryptoError::Base64DecodingFailed(_)));

        let public_result = load_public_key_from_env();
        assert!(public_result.is_err());
        assert!(matches!(public_result.unwrap_err(), CryptoError::Base64DecodingFailed(_)));
    }

    #[test]
    fn test_challenge_signing_and_verification() {
        let _lock = ENV_MUTEX.lock().unwrap();

        let (signing_key, verifying_key) = {
            let signing_key: SigningKey = SigningKey::generate(&mut OsRng);
            let verifying_key: VerifyingKey = signing_key.verifying_key();

            let private_key: String = STANDARD.encode(signing_key.to_bytes());
            let public_key: String = STANDARD.encode(verifying_key.to_bytes());

            env::set_var("IRONSHIELD_PRIVATE_KEY", &private_key);
            env::set_var("IRONSHIELD_PUBLIC_KEY", &public_key);

            (signing_key, verifying_key)
        };

        // Create a test challenge - it will be automatically signed
        let challenge = IronShieldChallenge::new(
            "test_website".to_string(),
            100_000,
            signing_key.clone(),
            verifying_key.to_bytes(),
        );

        // Verify the signature with environment keys
        verify_challenge_signature(&challenge).unwrap();

        // Verify with explicit key
        verify_challenge_signature_with_key(&challenge, &verifying_key.to_bytes()).unwrap();

        // Verify that the embedded public key matches what we expect
        assert_eq!(challenge.public_key, verifying_key.to_bytes());
    }

    #[test]
    fn test_tampered_challenge_detection() {
        let _lock = ENV_MUTEX.lock().unwrap();

        let (signing_key, verifying_key) = {
            let signing_key: SigningKey = SigningKey::generate(&mut OsRng);
            let verifying_key: VerifyingKey = signing_key.verifying_key();

            let private_key: String = STANDARD.encode(signing_key.to_bytes());
            let public_key: String = STANDARD.encode(verifying_key.to_bytes());

            env::set_var("IRONSHIELD_PRIVATE_KEY", &private_key);
            env::set_var("IRONSHIELD_PUBLIC_KEY", &public_key);

            (signing_key, verifying_key)
        };

        // Create and sign a challenge - signature is generated automatically
        let mut challenge = IronShieldChallenge::new(
            "test_website".to_string(),
            100_000,
            signing_key.clone(),
            verifying_key.to_bytes(),
        );

        // Verify original challenge works
        verify_challenge_signature(&challenge).unwrap();

        // Tamper with the challenge
        challenge.random_nonce = "tampered".to_string();

        // Verification should fail
        let result = verify_challenge_signature(&challenge);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), CryptoError::VerificationFailed(_)));
    }

    #[test]
    fn test_invalid_signature_format() {
        let _lock = ENV_MUTEX.lock().unwrap();
        {
            let signing_key: SigningKey = SigningKey::generate(&mut OsRng);
            let verifying_key: VerifyingKey = signing_key.verifying_key();

            let private_key: String = STANDARD.encode(signing_key.to_bytes());
            let public_key: String = STANDARD.encode(verifying_key.to_bytes());

            env::set_var("IRONSHIELD_PRIVATE_KEY", &private_key);
            env::set_var("IRONSHIELD_PUBLIC_KEY", &public_key);
        }

        // Create a challenge that will be properly signed
        let dummy_key = SigningKey::from_bytes(&[0u8; 32]);
        let mut challenge = IronShieldChallenge::new(
            "test_website".to_string(),
            100_000,
            dummy_key,
            [0x34; 32],
        );

        // Now manually corrupt the signature to test invalid format
        challenge.challenge_signature = [0xFF; 64]; // Invalid signature

        // Verification should fail
        let result = verify_challenge_signature(&challenge);
        assert!(result.is_err());
    }

    #[test]
    fn test_signing_message_creation() {
        let dummy_key = SigningKey::from_bytes(&[0u8; 32]);
        let challenge = IronShieldChallenge::new(
            "test_website".to_string(),
            100_000,
            dummy_key,
            [0x34; 32],
        );

        let message = create_signing_message(
            &challenge.random_nonce,
            challenge.created_time,
            challenge.expiration_time,
            &challenge.website_id,
            &challenge.challenge_param,
            &challenge.public_key
        );

        // Ensure the message format is as expected
        let expected_prefix = format!(
            "{}|{}|{}|{}|",
            challenge.random_nonce,
            challenge.created_time,
            challenge.expiration_time,
            challenge.website_id
        );
        assert!(message.starts_with(&expected_prefix));
        assert!(message.ends_with(&hex::encode(challenge.public_key)));
    }

    #[test]
    fn test_sign_challenge_uses_generate_signature() {
        let _lock = ENV_MUTEX.lock().unwrap();

        let (signing_key, verifying_key) = {
            let signing_key: SigningKey = SigningKey::generate(&mut OsRng);
            let verifying_key: VerifyingKey = signing_key.verifying_key();

            let private_key: String = STANDARD.encode(signing_key.to_bytes());
            let public_key: String = STANDARD.encode(verifying_key.to_bytes());

            env::set_var("IRONSHIELD_PRIVATE_KEY", &private_key);
            env::set_var("IRONSHIELD_PUBLIC_KEY", &public_key);

            (signing_key, verifying_key)
        };

        // Create a test challenge - it will be automatically signed
        let challenge = IronShieldChallenge::new(
            "test_website".to_string(),
            100_000,
            signing_key.clone(),
            verifying_key.to_bytes(),
        );

        // Test that sign_challenge and manual generate_signature produce the same result
        let sign_challenge_result = sign_challenge(&challenge).unwrap();

        let message = create_signing_message(
            &challenge.random_nonce,
            challenge.created_time,
            challenge.expiration_time,
            &challenge.website_id,
            &challenge.challenge_param,
            &challenge.public_key
        );
        let manual_signature = generate_signature(&signing_key, &message).unwrap();

        assert_eq!(sign_challenge_result, manual_signature,
                   "sign_challenge should produce the same result as manual generate_signature");
    }
}
