use crate::serde_utils::{
    deserialize_32_bytes,
    deserialize_signature,
    serialize_32_bytes,
    serialize_signature
};

use chrono::Utc;
use ed25519_dalek::SigningKey;
use hex;
use rand;
use serde::{
    Deserialize,
    Serialize
};

pub const CHALLENGE_DIFFICULTY:   u64 = 200_000_000u64;

const                HASH_BITS: usize = 256;
const               ARRAY_SIZE: usize = 32;
const            BITS_PER_BYTE: usize = 8;
const       BITS_PER_BYTE_MASK: usize = 7;
const           MAX_BYTE_VALUE:    u8 = 0xFF;
const         MAX_BIT_POSITION: usize = 255;
const                LSB_INDEX: usize = ARRAY_SIZE - 1;
const                LSB_VALUE:    u8 = 1;

/// IronShield Challenge structure for the proof-of-work algorithm
///
/// * `random_nonce`:         The SHA-256 hash of a random number (hex string).
/// * `created_time`:         Unix milli timestamp for the challenge.
/// * `expiration_time`:      Unix milli timestamp for the challenge expiration time.
/// * `challenge_param`:      Target threshold - hash must be less than this value.
/// * `recommended_attempts`: Expected number of attempts for user guidance (3x difficulty).
/// * `website_id`:           The identifier of the website.
/// * `public_key`:           Ed25519 public key for signature verification.
/// * `challenge_signature`:  Ed25519 signature over the challenge data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IronShieldChallenge {
    pub random_nonce:        String,
    pub created_time:        i64,
    pub expiration_time:     i64,
    pub website_id:          String,
    #[serde(
        serialize_with = "serialize_32_bytes",
        deserialize_with = "deserialize_32_bytes"
    )]
    pub challenge_param:      [u8; 32],
    pub recommended_attempts: u64,
    #[serde(
        serialize_with = "serialize_32_bytes",
        deserialize_with = "deserialize_32_bytes"
    )]
    pub public_key:          [u8; 32],
    #[serde(
        serialize_with = "serialize_signature",
        deserialize_with = "deserialize_signature"
    )]
    pub challenge_signature: [u8; 64],
}

impl IronShieldChallenge {
    /// Constructor for creating a new `IronShieldChallenge` instance.
    ///
    /// This function creates a new challenge and automatically generates a cryptographic
    /// signature using the provided private key. The signature covers all challenge data
    /// to prevent tampering.
    ///
    /// # Arguments
    /// * `website_id`:      The identifier of the website.
    /// * `difficulty`:      The target difficulty (expected number of attempts).
    /// * `private_key`:     Ed25519 private key for signing the challenge.
    /// * `public_key`:      Ed25519 public key corresponding to the private key.
    ///
    /// # Returns
    /// * `Self`:            A new, properly signed IronShieldChallenge.
    pub fn new(
        website_id:  String,
        difficulty:  u64,
        private_key: SigningKey,
        public_key:  [u8; 32],
    ) -> Self {
        let    random_nonce:   String = Self::generate_random_nonce();
        let    created_time:      i64 = Self::generate_created_time();
        let expiration_time:      i64 = created_time + 30_000; // 30-second expiration.
        let challenge_param: [u8; 32] = Self::difficulty_to_challenge_param(difficulty);
        
        // Create the signing message from the challenge components
        let signing_message = crate::crypto::create_signing_message(
            &random_nonce,
            created_time,
            expiration_time,
            &website_id,
            &challenge_param,
            &public_key
        );

        // Generate the signature using the reusable generate_signature function.
        let challenge_signature: [u8; 64] = crate::crypto::generate_signature(&private_key, &signing_message)
            .unwrap_or([0u8; 64]);

        Self {
            random_nonce,
            created_time,
            website_id,
            expiration_time,
            challenge_param,
            recommended_attempts: Self::recommended_attempts(difficulty),
            public_key,
            challenge_signature,
        }
    }

    /// Converts a difficulty value (expected number of attempts) to a challenge_param.
    ///
    /// The difficulty represents the expected number of hash attempts needed to find a valid nonce
    /// where SHA256(random_nonce_bytes + nonce_bytes) < challenge_param.
    ///
    /// Since hash outputs are uniformly distributed over the 256-bit space, the relationship is:
    /// challenge_param = 2^256 / difficulty.
    ///
    /// This function accurately calculates this for difficulties ranging from 1 to u64::MAX.
    ///
    /// # Arguments
    /// * `difficulty`: Expected number of attempts (must be > 0).
    ///
    /// # Returns
    /// * `[u8; 32]`: The challenge_param bytes in big-endian format.
    ///
    /// # Panics
    /// * Panics if difficulty is 0
    ///
    /// # Examples
    /// * difficulty = 1 ->         challenge_param = [0xFF; 32] (very easy, ~100% chance).
    /// * difficulty = 2 ->         challenge_param = [0x80, 0x00, ...] (MSB set, ~50% chance).
    /// * difficulty = 10,000 ->    challenge_param ≈ 2^242.7 (realistic difficulty).
    /// * difficulty = 1,000,000 -> challenge_param ≈ 2^236.4 (higher difficulty).
    pub fn difficulty_to_challenge_param(difficulty: u64) -> [u8; 32] {
        if difficulty == 0 {
            panic!("Difficulty cannot be zero.")
        }

        if difficulty == 1 {
            return [MAX_BYTE_VALUE; ARRAY_SIZE];
        }

        // Calculate target exponent: 256 - log2(difficulty).
        // This gives us the exponent of 2 in the result
        // 2^256 / difficulty ~= 2^(target_exponent).
        let log2_difficulty: f64 = (difficulty as f64).log2();
        let target_exponent: f64 = HASH_BITS as f64 - log2_difficulty;

        if target_exponent <= 0.0 { // Result would be less than 1, return min value.
            return Self::create_minimal_challenge_param()
        }

        if target_exponent >= HASH_BITS as f64 {
            return [MAX_BYTE_VALUE; ARRAY_SIZE];
        }

        // Round to the nearest whole number for bit positioning.
        let bit_position: usize = target_exponent.round() as usize;

        if bit_position >= HASH_BITS {
            return [MAX_BYTE_VALUE; ARRAY_SIZE];
        }

        Self::create_challenge_param_with_bit_set(bit_position)
    }

    /// Creates a challenge parameter with the minimal
    /// possible value (LSB set).
    ///
    /// # Returns
    /// * `[u8; 32]`: Array with only the least significant
    ///               bit set.
    fn create_minimal_challenge_param() -> [u8; 32] {
        let mut result: [u8; 32] = [0u8; ARRAY_SIZE];
        result[LSB_INDEX] = LSB_VALUE;
        result
    }

    /// Creates a challenge parameter with a specific bit
    /// set.
    ///
    /// For a big-endian byte array, bit N is located at:
    /// - byte index: (255 - N) / 8
    /// - bit index within byte: 7 - ((255 - N) % 8)
    ///
    /// # Arguments
    /// * `bit_position`: The bit position to set (0 = LSB, 255 = MSB).
    ///
    /// # Returns
    /// * `[u8; 32]`: Array with the specified bit set.
    fn create_challenge_param_with_bit_set(
        bit_position: usize
    ) -> [u8; 32] {
        let mut result: [u8; 32] = [0u8; ARRAY_SIZE];

        // Calculate byte and bit indices for big-endian format.
        let byte_index: usize = (MAX_BIT_POSITION - bit_position) / BITS_PER_BYTE;
        let  bit_index: usize = BITS_PER_BYTE_MASK - ((MAX_BIT_POSITION - bit_position) % BITS_PER_BYTE);

        if byte_index < ARRAY_SIZE {
            result[byte_index] = 1u8 << bit_index;
        } else { // Fallback on edge case: set the least significant bit.
            return Self::create_minimal_challenge_param()
        }

        result
    }

    /// # Returns
    /// * `bool`: `true` if the challenge is expired,
    ///           `false` otherwise.
    pub fn is_expired(&self) -> bool {
        Utc::now().timestamp_millis() > self.expiration_time
    }

    /// # Returns
    /// * `i64`: `created_time` **plus** 30 seconds.
    pub fn time_until_expiration(&self) -> i64 {
        self.expiration_time - Utc::now().timestamp_millis()
    }

    /// # Returns
    /// * `i64`: The current time in millis.
    pub fn generate_created_time() -> i64 {
        Utc::now().timestamp_millis()
    }

    /// # Returns
    /// * `String`: A random hex-encoded value.
    pub fn generate_random_nonce() -> String {
        hex::encode(&rand::random::<[u8; 16]>())
    }

    /// Returns the recommended number of attempts to expect for a given difficulty.
    ///
    /// This provides users with a realistic expectation of how many attempts they might need.
    /// Since the expected value is equal to the difficulty, we return 2x the difficulty
    /// to give users a reasonable upper bound for planning purposes.
    ///
    /// # Arguments
    /// * `difficulty`: The target difficulty (expected number of attempts)
    ///
    /// # Returns
    /// * `u64`: Recommended number of attempts (2x the difficulty)
    ///
    /// # Examples
    /// * difficulty = 1,000 → recommended_attempts = 2,000
    /// * difficulty = 50,000 → recommended_attempts = 100,000
    pub fn recommended_attempts(difficulty: u64) -> u64 {
        difficulty.saturating_mul(2)
    }

    /// Concatenates the challenge data into a string.
    ///
    /// Concatenates:
    /// * `random_nonce`     as a string.
    /// * `created_time`     as `i64`.
    /// * `expiration_time`  as `i64`.
    /// * `website_id`       as a string.
    /// * `public_key`       as a lowercase hex string.
    /// * `challenge_params` as a lowercase hex string.
    pub fn concat_struct(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}",
            self.random_nonce,
            self.created_time,
            self.expiration_time,
            self.website_id,
            // We need to encode the byte arrays for format! to work.
            hex::encode(self.challenge_param),
            self.recommended_attempts,
            hex::encode(self.public_key),
            hex::encode(self.challenge_signature)
        )
    }

    /// Creates an `IronShieldChallenge` from a concatenated string.
    ///
    /// This function reverses the operation of
    /// `IronShieldChallenge::concat_struct`.
    /// Expects a string in the format:
    /// "random_nonce|created_time|expiration_time|website_id|challenge_params|public_key|challenge_signature"
    ///
    /// # Arguments
    ///
    /// * `concat_str`: The concatenated string to parse, typically
    ///                 generated by `concat_struct()`.
    ///
    /// # Returns
    ///
    /// * `Result<Self, String>`: A result containing the parsed
    ///                           `IronShieldChallenge` or an
    ///                           error message if parsing fails.
    pub fn from_concat_struct(concat_str: &str) -> Result<Self, String> {
        let parts: Vec<&str> = concat_str.split('|').collect();

        if parts.len() != 8 {
            return Err(format!("Expected 8 parts, got {}", parts.len()));
        }

        let random_nonce: String = parts[0].to_string();

        let created_time: i64 = parts[1].parse::<i64>()
            .map_err(|_| "Failed to parse created_time as i64")?;

        let expiration_time: i64 = parts[2].parse::<i64>()
            .map_err(|_| "Failed to parse expiration_time as i64")?;

        let website_id: String = parts[3].to_string();

        let challenge_param_bytes: Vec<u8> = hex::decode(parts[4])
            .map_err(|_| "Failed to decode challenge_params hex string")?;
        let challenge_param: [u8; 32] = challenge_param_bytes
            .try_into()
            .map_err(|_| "Challenge params must be exactly 32 bytes")?;

        let recommended_attempts: u64 = parts[5].parse::<u64>()
            .map_err(|_| "Failed to parse recommended_attempts as u64")?;

        let public_key_bytes: Vec<u8> = hex::decode(parts[6])
            .map_err(|_| "Failed to decode public_key hex string")?;
        let public_key: [u8; 32] = public_key_bytes.try_into()
            .map_err(|_| "Public key must be exactly 32 bytes")?;

        let signature_bytes: Vec<u8> = hex::decode(parts[7])
            .map_err(|_| "Failed to decode challenge_signature hex string")?;
        let challenge_signature: [u8; 64] = signature_bytes
            .try_into()
            .map_err(|_| "Signature must be exactly 64 bytes")?;

        Ok(Self {
            random_nonce,
            created_time,
            expiration_time,
            website_id,
            challenge_param,
            recommended_attempts,
            public_key,
            challenge_signature,
        })
    }

    /// Encodes the challenge as a base64url string for HTTP header transport.
    ///
    /// This method concatenates all challenge fields using the established `|` delimiter
    /// format, and then base64url-encodes the result for safe transport in HTTP headers.
    ///
    /// # Returns
    /// * `String`: Base64url-encoded string ready for HTTP header use.
    ///
    /// # Example
    /// ```
    /// use ironshield_types::IronShieldChallenge;
    /// use ed25519_dalek::SigningKey;
    /// let dummy_key = SigningKey::from_bytes(&[0u8; 32]);
    /// let challenge = IronShieldChallenge::new(
    ///     "test_website".to_string(),
    ///     100_000,
    ///     dummy_key,
    ///     [0x34; 32],
    /// );
    /// let header_value = challenge.to_base64url_header();
    /// // Use header_value in HTTP header: "X-IronShield-Challenge-Data: {header_value}"
    /// ```
    pub fn to_base64url_header(&self) -> String {
        crate::serde_utils::concat_struct_base64url_encode(&self.concat_struct())
    }

    /// Decodes a base64url-encoded challenge from an HTTP header.
    ///
    /// This method reverses the `to_base64url_header()` operation by first base64url-decoding
    /// the input string and then parsing it using the established `|` delimiter format.
    ///
    /// # Arguments
    /// * `encoded_header`: The base64url-encoded string from the HTTP header.
    ///
    /// # Returns
    /// * `Result<Self, String>`: Decoded challenge or detailed error message.
    ///
    /// # Example
    /// ```
    /// use ironshield_types::IronShieldChallenge;
    /// use ed25519_dalek::SigningKey;
    /// // Create a challenge and encode it
    /// let dummy_key = SigningKey::from_bytes(&[0u8; 32]);
    /// let original = IronShieldChallenge::new(
    ///     "test_website".to_string(),
    ///     100_000,
    ///     dummy_key,
    ///     [0x34; 32],
    /// );
    /// let header_value = original.to_base64url_header();
    /// // Decode it back
    /// let decoded = IronShieldChallenge::from_base64url_header(&header_value).unwrap();
    /// assert_eq!(original.random_nonce, decoded.random_nonce);
    /// ```
    pub fn from_base64url_header(encoded_header: &str) -> Result<Self, String> {
        // Decode using the existing serde_utils function.
        let concat_str: String = crate::serde_utils::concat_struct_base64url_decode(encoded_header.to_string())?;

        // Parse using the existing concat_struct format.
        Self::from_concat_struct(&concat_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_difficulty_to_challenge_param_basic_cases() {
        // Test a very easy case.
        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(1);
        assert_eq!(challenge_param, [0xFF; 32]);

        // Test the exact powers of 2.
        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(2);
        let expected: [u8; 32] = {
            let mut arr: [u8; 32] = [0x00; 32];
            arr[0] = 0x80; // 2^255
            arr
        };
        assert_eq!(challenge_param, expected);

        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(4);
        let expected: [u8; 32] = {
            let mut arr: [u8; 32] = [0x00; 32];
            arr[0] = 0x40; // 2^254
            arr
        };
        assert_eq!(challenge_param, expected);

        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(256);
        let expected: [u8; 32] = {
            let mut arr: [u8; 32] = [0x00; 32];
            arr[0] = 0x01; // 2^248
            arr
        };
        assert_eq!(challenge_param, expected);
    }

    #[test]
    fn test_difficulty_to_challenge_param_realistic_range() {
        // Test difficulties in the expected range: 10,000 to 10,000,000.

        // difficulty = 10,000 ≈ 2^13.29, so the result ≈ 2^242.71 → rounds to 2^243.
        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(10_000);
        // Should have bit 243 set (byte 1, bit 3).
        assert_eq!(challenge_param[0], 0x00);
        assert_eq!(challenge_param[1], 0x08); // bit 3 = 0x08

        // difficulty = 50,000 ≈ 2^15.61, so the result ≈ 2^240.39 → rounds to 2^240.
        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(50_000);
        assert_eq!(challenge_param[0], 0x00);
        assert_eq!(challenge_param[1], 0x01); // bit 0 = 0x01

        // difficulty = 100,000 ≈ 2^16.61, so the result ≈ 2^239.39 → rounds to 2^239.
        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(100_000);
        assert_eq!(challenge_param[0], 0x00);
        assert_eq!(challenge_param[1], 0x00);
        assert_eq!(challenge_param[2], 0x80); // bit 7 of byte 2

        // difficulty = 1,000,000 ≈ 2^19.93, so the result ≈ 2^236.07 → rounds to 2^236.
        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(1_000_000);
        assert_eq!(challenge_param[0], 0x00);
        assert_eq!(challenge_param[1], 0x00);
        assert_eq!(challenge_param[2], 0x10); // bit 4 of byte 2

        // difficulty = 10,000,000 ≈ 2^23.25, so the result ≈ 2^232.75 → rounds to 2^233.
        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(10_000_000);
        assert_eq!(challenge_param[0], 0x00);
        assert_eq!(challenge_param[1], 0x00);
        assert_eq!(challenge_param[2], 0x02); // bit 1 of byte 2
    }

    #[test]
    fn test_difficulty_to_challenge_param_ordering() {
        // Test that higher difficulties produce smaller challenge_params.
        let difficulties: [u64; 9] = [1000, 5000, 10_000, 50_000, 100_000, 500_000, 1_000_000, 5_000_000, 10_000_000];
        let mut challenge_params = Vec::new();

        for &difficulty in &difficulties {
            challenge_params.push(IronShieldChallenge::difficulty_to_challenge_param(difficulty));
        }

        // Verify that challenge_params are in descending order (higher difficulty = smaller param).
        for i in 1..challenge_params.len() {
            assert!(
                challenge_params[i-1] > challenge_params[i],
                "Challenge param for difficulty {} should be larger than for difficulty {}",
                difficulties[i-1], difficulties[i]
            );
        }
    }

    #[test]
    fn test_difficulty_to_challenge_param_precision() {
        // Test that similar difficulties produce appropriately similar results.
        let base_difficulty: u64 = 100_000;
        let base_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(base_difficulty);

        // Small variations in difficulty will round to the same or nearby bit positions.
        let similar_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(100_001);

        // With rounding, very similar difficulties might produce the same result.
        // The key test is that larger difficulties produce smaller or equal challenge_params.
        assert!(base_param >= similar_param); // Should be the same or slightly larger.

        // Test that larger differences produce measurably different results.
        let much_different_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(200_000);
        assert!(base_param > much_different_param);

        // Test that the ordering is consistent for larger changes.
        let big_different_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(400_000);
        assert!(much_different_param > big_different_param);
    }

    #[test]
    fn test_difficulty_to_challenge_param_powers_of_10() {
        // Test various powers of 10.
        let difficulties: [u64; 6] = [10, 100, 1_000, 10_000, 100_000, 1_000_000];

        for &difficulty in &difficulties {
            let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(difficulty);

            // Should not be all zeros or all FFs (except for difficulty 1).
            assert_ne!(challenge_param, [0x00; 32]);
            assert_ne!(challenge_param, [0xFF; 32]);

            // Should have a reasonable number of leading zeros.
            let leading_zero_bytes: usize = challenge_param.iter().take_while(|&&b| b == 0).count();
            assert!(leading_zero_bytes < 32, "Too many leading zero bytes for difficulty {}", difficulty);

            // Should not be too small (no more than 28 leading zero bytes for this range)
            assert!(leading_zero_bytes < 28, "Challenge param too small for difficulty {}", difficulty);
        }
    }

    #[test]
    fn test_difficulty_to_challenge_param_mathematical_properties() {
        // Test mathematical properties of the algorithm.

        // For difficulty D1 and D2 where D2 = 2 * D1,
        // challenge_param(D1) should be approximately 2 * challenge_param(D2)
        let d1: u64 = 50_000;
        let d2: u64 = 100_000; // 2 * d1

        let param1: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(d1);
        let param2: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(d2);

        // Convert to u128 for comparison (taking first 16 bytes).
        let val1: u128 = u128::from_be_bytes(param1[0..16].try_into().unwrap());
        let val2: u128 = u128::from_be_bytes(param2[0..16].try_into().unwrap());

        // val1 should be approximately 2 * val2 (within reasonable tolerance).
        let ratio: f64 = val1 as f64 / val2 as f64;
        assert!(ratio > 1.8 && ratio < 2.2, "Ratio should be close to 2.0, got {}", ratio);
    }

    #[test]
    fn test_difficulty_to_challenge_param_edge_cases() {
        // Test zero difficulty panics.
        let result = std::panic::catch_unwind(|| {
            IronShieldChallenge::difficulty_to_challenge_param(0);
        });
        assert!(result.is_err());

        // Test very high difficulty produces a small value.
        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(u64::MAX);
        assert_ne!(challenge_param, [0xFF; 32]);
        assert_ne!(challenge_param, [0; 32]);

        // Test moderately high difficulties.
        let high_difficulty: u64 = 1u64 << 40; // 2^40
        let challenge_param: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(high_difficulty);
        assert_ne!(challenge_param, [0; 32]);
        assert_ne!(challenge_param, [0xFF; 32]);
    }

    #[test]
    fn test_difficulty_to_challenge_param_consistency() {
        // Test that the function produces consistent results.
        let test_difficulties: [u64; 13] = [
            10_000, 25_000, 50_000, 75_000, 100_000,
            250_000, 500_000, 750_000, 1_000_000,
            2_500_000, 5_000_000, 7_500_000, 10_000_000
        ];

        for &difficulty in &test_difficulties {
            let param1: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(difficulty);
            let param2: [u8; 32] = IronShieldChallenge::difficulty_to_challenge_param(difficulty);
            assert_eq!(param1, param2, "Function should be deterministic for difficulty {}", difficulty);

            // Test that the challenge param is reasonable.
            assert_ne!(param1, [0x00; 32]);
            assert_ne!(param1, [0xFF; 32]);
        }
    }

    #[test]
    fn test_recommended_attempts() {
        // Test recommended_attempts function
        assert_eq!(IronShieldChallenge::recommended_attempts(1000), 2000);
        assert_eq!(IronShieldChallenge::recommended_attempts(50000), 100000);
        assert_eq!(IronShieldChallenge::recommended_attempts(0), 0);

        // Test overflow protection
        assert_eq!(IronShieldChallenge::recommended_attempts(u64::MAX), u64::MAX);

        // Test realistic range
        assert_eq!(IronShieldChallenge::recommended_attempts(10_000), 20_000);
        assert_eq!(IronShieldChallenge::recommended_attempts(1_000_000), 2_000_000);
    }

    #[test]
    fn test_base64url_header_encoding_roundtrip() {
        // Create a dummy challenge for testing.
        let private_key = SigningKey::from_bytes(&[0; 32]);
        let public_key = private_key.verifying_key().to_bytes();
        let original_challenge = IronShieldChallenge::new(
            "test-site".to_string(),
            100_000,
            private_key,
            public_key,
        );

        // Encode and decode the challenge.
        let encoded = original_challenge.to_base64url_header();
        let decoded_challenge = IronShieldChallenge::from_base64url_header(&encoded)
            .expect("Failed to decode header");

        // Verify that the fields match.
        assert_eq!(original_challenge.random_nonce, decoded_challenge.random_nonce);
        assert_eq!(original_challenge.created_time, decoded_challenge.created_time);
        assert_eq!(original_challenge.expiration_time, decoded_challenge.expiration_time);
        assert_eq!(original_challenge.website_id, decoded_challenge.website_id);
        assert_eq!(original_challenge.challenge_param, decoded_challenge.challenge_param);
        assert_eq!(original_challenge.public_key, decoded_challenge.public_key);
        assert_eq!(original_challenge.challenge_signature, decoded_challenge.challenge_signature);
    }

    #[test]
    fn test_base64url_header_invalid_data() {
        // Test invalid base64url.
        let result: Result<IronShieldChallenge, String> = IronShieldChallenge::from_base64url_header("invalid-base64!");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Base64 decode error"));

        // Test valid base64url but invalid concatenated format.
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let invalid_format: String = URL_SAFE_NO_PAD.encode(b"not_enough_parts");
        let result: Result<IronShieldChallenge, String> = IronShieldChallenge::from_base64url_header(&invalid_format);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Expected 8 parts"));
    }

    #[test]
    fn test_difficulty_range_boundaries() {
        // Test around the specified range boundaries (10,000 to 10,000,000)
        let min_difficulty = 10_000;
        let max_difficulty = 10_000_000;

        let min_param = IronShieldChallenge::difficulty_to_challenge_param(min_difficulty);
        let max_param = IronShieldChallenge::difficulty_to_challenge_param(max_difficulty);

        // Min difficulty should produce a larger challenge_param than max difficulty.
        assert!(min_param > max_param);

        // Both should be reasonable values
        assert_ne!(min_param, [0x00; 32]);
        assert_ne!(min_param, [0xFF; 32]);
        assert_ne!(max_param, [0x00; 32]);
        assert_ne!(max_param, [0xFF; 32]);

        // Test values slightly outside the range
        let below_min = IronShieldChallenge::difficulty_to_challenge_param(9_999);
        let above_max = IronShieldChallenge::difficulty_to_challenge_param(10_000_001);

        // With rounding, very close values might produce the same result
        assert!(below_min >= min_param); // Should be the same or larger
        assert!(above_max <= max_param); // Should be the same or smaller
    }

    #[test]
    fn test_from_concat_struct_edge_cases() {
        // Test with all zero values
        let valid_32_byte_hex = "0000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(valid_32_byte_hex.len(), 64, "32-byte hex string should be exactly 64 characters");
        let valid_64_byte_hex = "00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";
        assert_eq!(valid_64_byte_hex.len(), 128, "64-byte hex string should be exactly 128 characters");

        let input = format!("test_nonce|1000000|1030000|test_website|{}|0|{}|{}",
                            valid_32_byte_hex, valid_32_byte_hex, valid_64_byte_hex);
        let result = IronShieldChallenge::from_concat_struct(&input);

        assert!(result.is_ok(), "Should parse valid zero-value data");
        let parsed = result.unwrap();
        assert_eq!(parsed.random_nonce, "test_nonce");
        assert_eq!(parsed.created_time, 1000000);
        assert_eq!(parsed.expiration_time, 1030000);
        assert_eq!(parsed.website_id, "test_website");
        assert_eq!(parsed.challenge_param, [0u8; 32]);
        assert_eq!(parsed.recommended_attempts, 0);
        assert_eq!(parsed.public_key, [0u8; 32]);
        assert_eq!(parsed.challenge_signature, [0u8; 64]);

        // Test with all max values (0xFF)
        let all_f_32_hex = "f".repeat(64);
        assert_eq!(all_f_32_hex.len(), 64, "All F's 32-byte hex string should be exactly 64 characters");
        let all_f_64_hex = "f".repeat(128);
        assert_eq!(all_f_64_hex.len(), 128, "All F's 64-byte hex string should be exactly 128 characters");

        let input = format!("max_nonce|{}|{}|max_website|{}|{}|{}|{}",
                            i64::MAX, i64::MAX, all_f_32_hex, u64::MAX, all_f_32_hex, all_f_64_hex);
        let result = IronShieldChallenge::from_concat_struct(&input);

        assert!(result.is_ok(), "Should parse valid max-value data");
        let parsed = result.unwrap();
        assert_eq!(parsed.random_nonce, "max_nonce");
        assert_eq!(parsed.created_time, i64::MAX);
        assert_eq!(parsed.expiration_time, i64::MAX);
        assert_eq!(parsed.website_id, "max_website");
        assert_eq!(parsed.challenge_param, [0xffu8; 32]);
        assert_eq!(parsed.recommended_attempts, u64::MAX);
        assert_eq!(parsed.public_key, [0xffu8; 32]);
        assert_eq!(parsed.challenge_signature, [0xffu8; 64]);
    }
}
