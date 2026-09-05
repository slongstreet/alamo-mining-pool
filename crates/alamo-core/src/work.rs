//! A block template processed into everything the stratum server and validator need.

use crate::encode::write_varint;
use crate::hash::Hash256;
use crate::header::BlockHeader;
use crate::job::JobId;
use crate::merkle;
use crate::target::Target;
use crate::Algorithm;

/// A non-coinbase transaction from the template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateTx {
    /// Raw serialized transaction (with witness data if any), as the node gave it.
    pub data: Vec<u8>,
    /// Transaction id in internal byte order.
    pub txid: Hash256,
}

/// A block template ready to be turned into per-worker jobs.
#[derive(Clone, Debug)]
pub struct WorkTemplate {
    /// Monotonic id assigned by the template source.
    pub id: JobId,
    /// Ticker of the chain this template is for.
    pub coin: String,
    /// Proof-of-work algorithm.
    pub algorithm: Algorithm,
    /// Height of the block being mined.
    pub height: u64,
    /// Block version, as given by the node (includes version bits).
    pub version: i32,
    /// Previous block hash, internal byte order.
    pub prev_hash: Hash256,
    /// Compact target.
    pub bits: u32,
    /// Network target.
    pub target: Target,
    /// Node's current time for the header.
    pub cur_time: u32,
    /// Earliest acceptable header time.
    pub min_time: u32,
    /// Coinbase reward including fees, in base units.
    pub coinbase_value: u64,
    /// Bytes placed at the start of the coinbase scriptSig (BIP34 height, tag, and later
    /// the aux commitment). The extranonce follows immediately after.
    pub coinbase_script_prefix: Vec<u8>,
    /// Witness commitment scriptPubKey to add as a coinbase output, if the node gave one.
    pub witness_commitment: Option<Vec<u8>>,
    /// Non-coinbase transactions in block order.
    pub transactions: Vec<TemplateTx>,
    /// Merkle branch for the coinbase, derived from `transactions`.
    pub merkle_branch: Vec<Hash256>,
    /// Bytes appended after the transactions in the serialized block (Litecoin MWEB).
    pub extra_payload: Vec<u8>,
    /// Whether miners must drop work from earlier templates (new chain tip).
    pub clean_jobs: bool,
    /// Unix time this template was built.
    pub created_at: u64,
}

impl WorkTemplate {
    /// Compute the merkle branch from the transaction list. Call after filling `transactions`.
    pub fn compute_merkle_branch(&mut self) {
        let txids: Vec<Hash256> = self.transactions.iter().map(|t| t.txid).collect();
        self.merkle_branch = merkle::coinbase_branch(&txids);
    }

    /// Merkle root for a given coinbase txid.
    pub fn merkle_root(&self, coinbase_txid: &Hash256) -> Hash256 {
        merkle::root_from_branch(coinbase_txid, &self.merkle_branch)
    }

    /// Network difficulty relative to pool difficulty 1.
    pub fn network_difficulty(&self) -> f64 {
        self.target.difficulty()
    }

    /// Serialize a full block: header, transaction count, coinbase, transactions, payload.
    pub fn assemble_block(&self, header: &BlockHeader, coinbase: &[u8]) -> Vec<u8> {
        let tx_bytes: usize = self.transactions.iter().map(|t| t.data.len()).sum();
        let mut out =
            Vec::with_capacity(80 + 9 + coinbase.len() + tx_bytes + self.extra_payload.len());
        out.extend_from_slice(&header.serialize());
        write_varint(&mut out, 1 + self.transactions.len() as u64);
        out.extend_from_slice(coinbase);
        for tx in &self.transactions {
            out.extend_from_slice(&tx.data);
        }
        out.extend_from_slice(&self.extra_payload);
        out
    }
}
