//! Xorshift128+ — non-cryptographic PRNG (Vigna). Used for ULID entropy
//! and randomized test generation. Deliberately not `rand`: spec Appendix B.

use std::ops::Range;

pub struct Xorshift128(pub [u64; 2]);

/// SplitMix64 — seeds the xorshift state so nearby seeds produce unrelated streams.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Xorshift128 {
    pub fn new(seed: u64) -> Self {
        let mut s = seed;
        let a = splitmix64(&mut s);
        let b = splitmix64(&mut s);
        // xorshift state must never be all-zero
        Xorshift128([a | 1, b])
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut s1 = self.0[0];
        let s0 = self.0[1];
        let result = s0.wrapping_add(s1);
        self.0[0] = s0;
        s1 ^= s1 << 23;
        self.0[1] = s1 ^ s0 ^ (s1 >> 18) ^ (s0 >> 5);
        result
    }

    /// Modulo bias is acceptable for our uses (tests, ULID entropy).
    pub fn gen_range(&mut self, range: Range<u64>) -> u64 {
        debug_assert!(range.start < range.end);
        range.start + self.next_u64() % (range.end - range.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Xorshift128::new(42);
        let mut b = Xorshift128::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Xorshift128::new(1);
        let mut b = Xorshift128::new(2);
        let same = (0..100).filter(|_| a.next_u64() == b.next_u64()).count();
        assert!(same < 3);
    }

    #[test]
    fn zero_seed_is_not_degenerate() {
        let mut r = Xorshift128::new(0);
        let vals: Vec<u64> = (0..10).map(|_| r.next_u64()).collect();
        assert!(vals.iter().any(|&v| v != 0));
        assert!(vals.windows(2).any(|w| w[0] != w[1]));
    }

    #[test]
    fn gen_range_stays_in_bounds_and_covers() {
        let mut r = Xorshift128::new(7);
        let mut seen = [false; 8];
        for _ in 0..10_000 {
            let v = r.gen_range(0..8);
            assert!(v < 8);
            seen[v as usize] = true;
        }
        assert!(seen.iter().all(|&s| s), "all 8 buckets hit over 10k draws");
    }
}
