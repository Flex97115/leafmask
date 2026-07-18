//! Deterministic hash-based value generation (feature `transform.hash-engine`).
//!
//! Higher-level transformers derive their pseudo-random replacement values from
//! this engine so that masking a given input always yields the same output
//! across separate dump runs, on any machine. Determinism comes purely from
//! `SHA-256(salt || input)`; there is no RNG state and no time input.

use sha2::{Digest, Sha256};

/// A deterministic generator seeded by a salt.
#[derive(Debug, Clone)]
pub struct HashEngine {
    salt: Vec<u8>,
}

impl HashEngine {
    /// Create an engine with the given salt/seed. Different salts yield
    /// different output for the same input.
    pub fn new(salt: impl Into<Vec<u8>>) -> Self {
        HashEngine { salt: salt.into() }
    }

    /// The raw 32-byte SHA-256 digest of `salt || input`.
    pub fn digest(&self, input: &[u8]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(&self.salt);
        h.update(input);
        h.finalize().into()
    }

    /// Produce `n` deterministic bytes. For `n > 32` the stream is extended by
    /// hashing `salt || input || counter` blocks, so arbitrary lengths stay
    /// deterministic.
    pub fn hash_bytes(&self, input: &[u8], n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        let mut counter: u32 = 0;
        while out.len() < n {
            let mut h = Sha256::new();
            h.update(&self.salt);
            h.update(input);
            h.update(counter.to_be_bytes());
            out.extend_from_slice(&h.finalize());
            counter += 1;
        }
        out.truncate(n);
        out
    }

    /// Project the hash into an unsigned 64-bit integer.
    pub fn hash_u64(&self, input: &[u8]) -> u64 {
        let d = self.digest(input);
        u64::from_be_bytes(d[..8].try_into().unwrap())
    }

    /// Project the hash into a signed 64-bit integer (always non-negative so it
    /// is safe to use as an index or magnitude by callers).
    pub fn hash_i64(&self, input: &[u8]) -> i64 {
        (self.hash_u64(input) >> 1) as i64
    }

    /// Deterministically map the input into the inclusive range `[min, max]`.
    /// Returns `min` when the range is degenerate/invalid.
    pub fn ranged_i64(&self, input: &[u8], min: i64, max: i64) -> i64 {
        if max <= min {
            return min;
        }
        // width fits in u64 for any i64 pair.
        let width = (max as i128 - min as i128 + 1) as u128;
        let r = (self.hash_u64(input) as u128) % width;
        (min as i128 + r as i128) as i64
    }

    /// Pick a deterministic index into a slice of length `len`.
    pub fn index(&self, input: &[u8], len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        (self.hash_u64(input) % len as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Acceptance: same input + salt always produce the same output.
    #[test]
    fn deterministic_across_instances() {
        let a = HashEngine::new("pepper");
        let b = HashEngine::new("pepper");
        assert_eq!(
            a.digest(b"alice@example.com"),
            b.digest(b"alice@example.com")
        );
        assert_eq!(a.hash_bytes(b"x", 40), b.hash_bytes(b"x", 40));
        assert_eq!(a.hash_i64(b"x"), b.hash_i64(b"x"));
        assert_eq!(a.ranged_i64(b"x", 1, 100), b.ranged_i64(b"x", 1, 100));
    }

    // Acceptance: different salts/seeds produce different output for same input.
    #[test]
    fn salt_changes_output() {
        let a = HashEngine::new("salt-a");
        let b = HashEngine::new("salt-b");
        assert_ne!(a.digest(b"same"), b.digest(b"same"));
        assert_ne!(a.hash_i64(b"same"), b.hash_i64(b"same"));
    }

    // Acceptance: project a hash into different target ranges/types.
    #[test]
    fn projects_into_ranges_and_types() {
        let e = HashEngine::new("seed");

        // bytes of arbitrary length, deterministic and correctly sized.
        assert_eq!(e.hash_bytes(b"v", 12).len(), 12);
        assert_eq!(e.hash_bytes(b"v", 64).len(), 64);

        // i64 is non-negative.
        assert!(e.hash_i64(b"v") >= 0);

        // ranged projection stays within bounds for many inputs.
        for i in 0..1000u32 {
            let r = e.ranged_i64(&i.to_be_bytes(), 10, 20);
            assert!((10..=20).contains(&r), "out of range: {r}");
        }
        // degenerate range returns the lower bound.
        assert_eq!(e.ranged_i64(b"v", 5, 5), 5);
        // index stays in bounds.
        assert!(e.index(b"v", 7) < 7);
    }

    // Sanity: the ranged projection actually covers its full span.
    #[test]
    fn ranged_covers_span() {
        let e = HashEngine::new("seed");
        let mut seen = std::collections::HashSet::new();
        for i in 0..500u32 {
            seen.insert(e.ranged_i64(&i.to_be_bytes(), 0, 3));
        }
        assert_eq!(seen, [0, 1, 2, 3].into_iter().collect());
    }
}
