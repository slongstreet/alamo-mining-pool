//! 256-bit proof-of-work targets, compact encoding, and difficulty conversions.

use crate::hash::Hash256;
use primitive_types::U256;
use std::fmt;

/// A 256-bit proof-of-work target. A hash meets the target when, interpreted as a
/// little-endian integer, it is less than or equal to the target.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Target(U256);

impl Target {
    /// The network difficulty-1 target: `0x00000000FFFF0000...0000` (compact `0x1d00ffff`).
    ///
    /// Network difficulty is measured relative to this target for every coin, regardless
    /// of the coin's own proof-of-work limit. Stratum share difficulty is a per-algorithm
    /// multiple of it: see [`crate::algo::Algorithm::share_target`], which applies the
    /// 2^16 factor scrypt miners expect.
    pub const DIFF1: Target = Target(U256([0, 0, 0, 0x0000_0000_ffff_0000]));

    /// Build a target from its compact ("bits") encoding as used in block headers.
    pub fn from_compact(bits: u32) -> Self {
        let size = bits >> 24;
        let word = bits & 0x007f_ffff;
        let value = if size <= 3 {
            U256::from(word >> (8 * (3 - size)))
        } else {
            U256::from(word) << (8 * (size - 3))
        };
        Target(value)
    }

    /// Encode this target in compact form. The negative flag is never set.
    pub fn to_compact(self) -> u32 {
        let value = self.0;
        if value.is_zero() {
            return 0;
        }
        let mut size = (value.bits() as u32).div_ceil(8);
        let mut compact = if size <= 3 {
            (value.low_u64() << (8 * (3 - size))) as u32
        } else {
            (value >> (8 * (size - 3))).low_u32()
        };
        if compact & 0x0080_0000 != 0 {
            compact >>= 8;
            size += 1;
        }
        compact | (size << 24)
    }

    /// The target corresponding to a pool difficulty relative to [`Target::DIFF1`].
    ///
    /// Difficulty is carried with 32 fractional bits so sub-1 and non-integer
    /// difficulties are handled without floating-point drift in the top bits.
    pub fn from_difficulty(difficulty: f64) -> Self {
        assert!(
            difficulty > 0.0 && difficulty.is_finite(),
            "difficulty must be positive"
        );
        let scaled = (difficulty * 4_294_967_296.0).max(1.0) as u128;
        Target((Self::DIFF1.0 << 32) / U256::from(scaled))
    }

    /// The pool difficulty of this target relative to [`Target::DIFF1`].
    pub fn difficulty(self) -> f64 {
        if self.0.is_zero() {
            return f64::INFINITY;
        }
        u256_to_f64(Self::DIFF1.0) / u256_to_f64(self.0)
    }

    /// Whether the given hash (internal byte order) satisfies this target.
    pub fn is_met_by(self, hash: &Hash256) -> bool {
        U256::from_little_endian(hash) <= self.0
    }

    /// The raw 256-bit value.
    pub fn as_u256(self) -> U256 {
        self.0
    }
}

/// The pool difficulty achieved by a hash, relative to [`Target::DIFF1`].
pub fn hash_difficulty(hash: &Hash256) -> f64 {
    let value = U256::from_little_endian(hash);
    if value.is_zero() {
        return f64::INFINITY;
    }
    u256_to_f64(Target::DIFF1.0) / u256_to_f64(value)
}

fn u256_to_f64(value: U256) -> f64 {
    value.0.iter().rev().fold(0.0, |acc, limb| {
        acc * 18_446_744_073_709_551_616.0 + *limb as f64
    })
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Target({:#066x})", self.0)
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:064x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff1_matches_bitcoin_compact() {
        assert_eq!(Target::from_compact(0x1d00_ffff), Target::DIFF1);
        assert_eq!(Target::DIFF1.to_compact(), 0x1d00_ffff);
        assert_eq!(
            Target::DIFF1.to_string(),
            "00000000ffff0000000000000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn compact_round_trips() {
        for bits in [
            0x1d00_ffffu32,
            0x1e0f_fff0,
            0x1b04_04cb,
            0x1703_a30c,
            0x0300_ffff,
        ] {
            assert_eq!(
                Target::from_compact(bits).to_compact(),
                bits,
                "bits {bits:#x}"
            );
        }
        // A mantissa that shifts entirely out of range decodes to zero, as in Bitcoin Core.
        assert_eq!(
            Target::from_compact(0x0100_0001).as_u256(),
            primitive_types::U256::zero()
        );
    }

    #[test]
    fn scrypt_pow_limit_is_diff1_over_4096_ish() {
        // Litecoin's genesis bits 0x1e0ffff0 is diff1 shifted up by 12 bits with a
        // slightly smaller mantissa. Its pool difficulty is well below 1.
        let d = Target::from_compact(0x1e0f_fff0).difficulty();
        assert!(d > 0.000244 && d < 0.000245, "got {d}");
    }

    #[test]
    fn difficulty_round_trip() {
        for diff in [1.0, 2.0, 1024.0, 65536.0, 0.5, 3.75, 1e9] {
            let t = Target::from_difficulty(diff);
            let back = t.difficulty();
            let rel = (back - diff).abs() / diff;
            assert!(rel < 1e-6, "diff {diff}: got {back}");
        }
    }

    #[test]
    fn higher_difficulty_means_smaller_target() {
        assert!(Target::from_difficulty(1024.0) < Target::from_difficulty(1.0));
    }

    #[test]
    fn hash_difficulty_of_diff1_boundary() {
        // 0x00000000ffff0000... as a number; internal order is little-endian, so the
        // big-endian bytes 4 and 5 land at indices 27 and 26.
        let mut hash = [0u8; 32];
        hash[27] = 0xff;
        hash[26] = 0xff;
        assert!(Target::DIFF1.is_met_by(&hash));
        let d = hash_difficulty(&hash);
        assert!((d - 1.0).abs() < 1e-12, "got {d}");
        // One unit in big-endian byte 3 pushes the value above diff1.
        hash[28] = 0x01;
        assert!(!Target::DIFF1.is_met_by(&hash));
        assert!(hash_difficulty(&hash) < 1.0);
    }
}
