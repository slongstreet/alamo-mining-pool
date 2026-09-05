//! Coinbase transaction construction, split around the extranonce for stratum.

use crate::encode::{push_data, write_varint};
use crate::hash::Hash256;
use crate::work::WorkTemplate;
use sha2::{Digest, Sha256};

/// Bitcoin consensus limit on the coinbase scriptSig length.
pub const MAX_COINBASE_SCRIPT_LEN: usize = 100;

/// Why a coinbase could not be built.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CoinbaseError {
    /// The scriptSig would exceed the consensus limit.
    #[error("coinbase scriptSig would be {0} bytes, limit is 100")]
    ScriptTooLong(usize),
    /// The payout script is empty.
    #[error("payout script is empty")]
    EmptyPayout,
}

/// A coinbase transaction split into the bytes before and after the extranonce.
///
/// `coinb1 || extranonce1 || extranonce2 || coinb2` is the non-witness serialization whose
/// double-SHA256 is the txid used in the merkle tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoinbaseParts {
    /// Bytes before the extranonce.
    pub coinb1: Vec<u8>,
    /// Bytes after the extranonce.
    pub coinb2: Vec<u8>,
    /// Total extranonce length (extranonce1 + extranonce2) the parts were built for.
    pub extranonce_len: usize,
    /// Whether the block serialization needs the witness reserved value on this coinbase.
    pub witness: bool,
}

impl CoinbaseParts {
    /// Build the coinbase for `work`, paying `payout_script`. The scriptSig is the
    /// template's prefix, then `aux_commitment` as a data push (if not empty), then
    /// `extranonce_len` bytes left for the extranonce.
    pub fn build(
        work: &WorkTemplate,
        payout_script: &[u8],
        aux_commitment: &[u8],
        extranonce_len: usize,
    ) -> Result<Self, CoinbaseError> {
        if payout_script.is_empty() {
            return Err(CoinbaseError::EmptyPayout);
        }
        let commitment_len = if aux_commitment.is_empty() {
            0
        } else {
            1 + aux_commitment.len()
        };
        let script_len = work.coinbase_script_prefix.len() + commitment_len + extranonce_len;
        if script_len > MAX_COINBASE_SCRIPT_LEN {
            return Err(CoinbaseError::ScriptTooLong(script_len));
        }

        let mut coinb1 = Vec::with_capacity(4 + 1 + 36 + 1 + script_len);
        coinb1.extend_from_slice(&1i32.to_le_bytes()); // version
        coinb1.push(1); // input count
        coinb1.extend_from_slice(&[0u8; 32]); // null prevout hash
        coinb1.extend_from_slice(&0xffff_ffffu32.to_le_bytes()); // prevout index
        write_varint(&mut coinb1, script_len as u64);
        coinb1.extend_from_slice(&work.coinbase_script_prefix);
        if !aux_commitment.is_empty() {
            push_data(&mut coinb1, aux_commitment);
        }

        let mut coinb2 = Vec::with_capacity(4 + 1 + 9 + payout_script.len() + 48 + 4);
        coinb2.extend_from_slice(&0xffff_ffffu32.to_le_bytes()); // sequence
        let outputs = 1 + usize::from(work.witness_commitment.is_some());
        write_varint(&mut coinb2, outputs as u64);
        coinb2.extend_from_slice(&work.coinbase_value.to_le_bytes());
        write_varint(&mut coinb2, payout_script.len() as u64);
        coinb2.extend_from_slice(payout_script);
        if let Some(commitment) = &work.witness_commitment {
            coinb2.extend_from_slice(&0u64.to_le_bytes());
            write_varint(&mut coinb2, commitment.len() as u64);
            coinb2.extend_from_slice(commitment);
        }
        coinb2.extend_from_slice(&0u32.to_le_bytes()); // locktime

        Ok(Self {
            coinb1,
            coinb2,
            extranonce_len,
            witness: work.witness_commitment.is_some(),
        })
    }

    /// Non-witness serialization for a given extranonce (the txid preimage).
    pub fn serialize(&self, extranonce: &[u8]) -> Vec<u8> {
        debug_assert_eq!(extranonce.len(), self.extranonce_len);
        let mut out = Vec::with_capacity(self.coinb1.len() + extranonce.len() + self.coinb2.len());
        out.extend_from_slice(&self.coinb1);
        out.extend_from_slice(extranonce);
        out.extend_from_slice(&self.coinb2);
        out
    }

    /// Transaction id (internal byte order) for a given extranonce.
    pub fn txid(&self, extranonce: &[u8]) -> Hash256 {
        let first = Sha256::new()
            .chain_update(&self.coinb1)
            .chain_update(extranonce)
            .chain_update(&self.coinb2)
            .finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&Sha256::digest(first));
        out
    }

    /// Serialization to place in the block. Adds the segwit marker, flag, and the
    /// all-zero witness reserved value when a witness commitment is present.
    pub fn serialize_for_block(&self, extranonce: &[u8]) -> Vec<u8> {
        if !self.witness {
            return self.serialize(extranonce);
        }
        let body_end = self.coinb2.len() - 4;
        let mut out =
            Vec::with_capacity(self.coinb1.len() + extranonce.len() + self.coinb2.len() + 36);
        out.extend_from_slice(&self.coinb1[..4]);
        out.extend_from_slice(&[0x00, 0x01]);
        out.extend_from_slice(&self.coinb1[4..]);
        out.extend_from_slice(extranonce);
        out.extend_from_slice(&self.coinb2[..body_end]);
        out.push(1); // one witness item
        push_data(&mut out, &[0u8; 32]);
        out.extend_from_slice(&self.coinb2[body_end..]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::sha256d;

    fn sample_work(witness: bool) -> WorkTemplate {
        WorkTemplate::regtest_sample(
            1_000,
            witness.then(|| {
                hex::decode(
                    "6a24aa21a9ede2f61c3f71d1defd3fa999dfa36953755c690689799962b48bebd836974e8cf9",
                )
                .unwrap()
            }),
        )
    }

    #[test]
    fn layout_without_witness() {
        let work = sample_work(false);
        let payout = vec![0x00, 0x14]
            .into_iter()
            .chain([0x42u8; 20])
            .collect::<Vec<_>>();
        let parts = CoinbaseParts::build(&work, &payout, &[], 8).unwrap();
        let en = [0xab; 8];
        let tx = parts.serialize(&en);
        // version 1, one input, null prevout, script len
        assert_eq!(&tx[..4], &[1, 0, 0, 0]);
        assert_eq!(tx[4], 1);
        assert_eq!(&tx[5..37], &[0; 32]);
        assert_eq!(&tx[37..41], &[0xff; 4]);
        let script_len = tx[41] as usize;
        assert_eq!(script_len, work.coinbase_script_prefix.len() + 8);
        let script = &tx[42..42 + script_len];
        assert!(script.starts_with(&work.coinbase_script_prefix));
        assert!(script.ends_with(&en));
        let mut i = 42 + script_len;
        assert_eq!(&tx[i..i + 4], &[0xff; 4]); // sequence
        i += 4;
        assert_eq!(tx[i], 1); // one output
        i += 1;
        assert_eq!(
            u64::from_le_bytes(tx[i..i + 8].try_into().unwrap()),
            5_000_000_000
        );
        i += 8;
        assert_eq!(tx[i] as usize, payout.len());
        i += 1;
        assert_eq!(&tx[i..i + payout.len()], &payout[..]);
        i += payout.len();
        assert_eq!(&tx[i..i + 4], &[0; 4]); // locktime
        assert_eq!(tx.len(), i + 4);
        assert_eq!(parts.serialize_for_block(&en), tx);
    }

    #[test]
    fn witness_form_wraps_the_same_body() {
        let work = sample_work(true);
        let payout = vec![0x76, 0xa9, 0x14]
            .into_iter()
            .chain([0x42u8; 20])
            .chain([0x88, 0xac])
            .collect::<Vec<_>>();
        let parts = CoinbaseParts::build(&work, &payout, &[], 8).unwrap();
        let en = [0x01; 8];
        let plain = parts.serialize(&en);
        let witness = parts.serialize_for_block(&en);
        assert_eq!(witness.len(), plain.len() + 2 + 1 + 1 + 32);
        assert_eq!(&witness[..4], &plain[..4]);
        assert_eq!(&witness[4..6], &[0x00, 0x01]);
        assert_eq!(&witness[6..6 + plain.len() - 8], &plain[4..plain.len() - 4]);
        let w = &witness[plain.len() - 2..];
        assert_eq!(&w[..2], &[0x01, 0x20]);
        assert_eq!(&w[2..34], &[0u8; 32]);
        assert_eq!(&w[34..], &[0; 4]);
        // Two outputs: payout and commitment.
        let outputs_index = 42 + plain[41] as usize + 4;
        assert_eq!(plain[outputs_index], 2);
        // txid is over the non-witness form.
        assert_eq!(parts.txid(&en), sha256d(&plain));
    }

    #[test]
    fn rejects_overlong_script() {
        let mut work = sample_work(false);
        work.coinbase_script_prefix = vec![0; 95];
        assert_eq!(
            CoinbaseParts::build(&work, &[0x51], &[], 8),
            Err(CoinbaseError::ScriptTooLong(103))
        );
        assert_eq!(
            CoinbaseParts::build(&work, &[], &[], 4),
            Err(CoinbaseError::EmptyPayout)
        );
        // A 44-byte commitment costs 45 bytes of script: 47 + 45 + 8 fits, 48 does not.
        work.coinbase_script_prefix = vec![0; 47];
        assert!(CoinbaseParts::build(&work, &[0x51], &[0xfa; 44], 8).is_ok());
        work.coinbase_script_prefix = vec![0; 48];
        assert_eq!(
            CoinbaseParts::build(&work, &[0x51], &[0xfa; 44], 8),
            Err(CoinbaseError::ScriptTooLong(101))
        );
    }

    #[test]
    fn aux_commitment_is_pushed_between_prefix_and_extranonce() {
        let work = sample_work(false);
        let commitment = [0xfa; 44];
        let parts = CoinbaseParts::build(&work, &[0x51], &commitment, 8).unwrap();
        let en = [0xee; 8];
        let tx = parts.serialize(&en);
        let script_len = tx[41] as usize;
        assert_eq!(script_len, work.coinbase_script_prefix.len() + 45 + 8);
        let script = &tx[42..42 + script_len];
        let prefix_len = work.coinbase_script_prefix.len();
        assert_eq!(&script[..prefix_len], &work.coinbase_script_prefix[..]);
        assert_eq!(script[prefix_len], 44);
        assert_eq!(&script[prefix_len + 1..prefix_len + 45], &commitment);
        assert_eq!(&script[prefix_len + 45..], &en);
        // Extranonce-free form (an aux coinbase) is a complete transaction.
        let aux = CoinbaseParts::build(&work, &[0x51], &[], 0).unwrap();
        assert_eq!(aux.extranonce_len, 0);
        assert_eq!(
            aux.serialize(&[]).len(),
            aux.coinb1.len() + aux.coinb2.len()
        );
    }
}
