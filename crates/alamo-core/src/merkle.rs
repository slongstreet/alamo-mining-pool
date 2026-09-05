//! Merkle tree helpers in Bitcoin's (duplicate-last-odd-node) style.

use crate::hash::{sha256d, Hash256};

fn hash_pair(a: &Hash256, b: &Hash256) -> Hash256 {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(a);
    buf[32..].copy_from_slice(b);
    sha256d(&buf)
}

/// The stratum "merkle branch" for the coinbase (leaf index 0), given the hashes of every
/// other transaction in block order. Combining the coinbase hash with these steps in order
/// yields the merkle root.
pub fn coinbase_branch(tx_hashes: &[Hash256]) -> Vec<Hash256> {
    let mut steps = Vec::new();
    // Level 0: a placeholder for the coinbase followed by the real transactions.
    let mut level: Vec<Option<Hash256>> = std::iter::once(None)
        .chain(tx_hashes.iter().copied().map(Some))
        .collect();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        steps.push(level[1].expect("index 1 is never the coinbase placeholder"));
        let mut next = vec![None];
        for pair in level[2..].chunks(2) {
            next.push(Some(hash_pair(&pair[0].unwrap(), &pair[1].unwrap())));
        }
        level = next;
    }
    steps
}

/// Fold the coinbase hash through a branch to produce the merkle root.
pub fn root_from_branch(coinbase_hash: &Hash256, branch: &[Hash256]) -> Hash256 {
    branch
        .iter()
        .fold(*coinbase_hash, |acc, step| hash_pair(&acc, step))
}

/// Fold a leaf hash at `index` through a branch to produce the merkle root, the way
/// `CAuxPow::CheckMerkleBranch` does: odd indices hash on the right.
pub fn root_from_branch_at(leaf: &Hash256, branch: &[Hash256], mut index: usize) -> Hash256 {
    let mut hash = *leaf;
    for step in branch {
        hash = if index & 1 == 1 {
            hash_pair(step, &hash)
        } else {
            hash_pair(&hash, step)
        };
        index >>= 1;
    }
    hash
}

/// The merkle branch for the leaf at `index` in a tree over `hashes`.
pub fn branch_at(hashes: &[Hash256], mut index: usize) -> Vec<Hash256> {
    assert!(index < hashes.len(), "leaf index out of range");
    let mut steps = Vec::new();
    let mut level = hashes.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        steps.push(level[index ^ 1]);
        level = level.chunks(2).map(|p| hash_pair(&p[0], &p[1])).collect();
        index >>= 1;
    }
    steps
}

/// Full merkle root over an ordered list of transaction hashes (coinbase first).
pub fn merkle_root(hashes: &[Hash256]) -> Hash256 {
    assert!(!hashes.is_empty(), "a block has at least a coinbase");
    let mut level = hashes.to_vec();
    while level.len() > 1 {
        if level.len() % 2 == 1 {
            level.push(*level.last().unwrap());
        }
        level = level.chunks(2).map(|p| hash_pair(&p[0], &p[1])).collect();
    }
    level[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(i: u8) -> Hash256 {
        let mut x = [0u8; 32];
        x[0] = i;
        x
    }

    #[test]
    fn single_tx_root_is_itself() {
        assert_eq!(merkle_root(&[h(1)]), h(1));
        assert!(coinbase_branch(&[]).is_empty());
        assert_eq!(root_from_branch(&h(1), &[]), h(1));
    }

    #[test]
    fn branch_agrees_with_full_root() {
        for n in 1..=9usize {
            let txs: Vec<Hash256> = (1..=n as u8).map(h).collect();
            let branch = coinbase_branch(&txs);
            let all: Vec<Hash256> = std::iter::once(h(0)).chain(txs.iter().copied()).collect();
            assert_eq!(root_from_branch(&h(0), &branch), merkle_root(&all), "n={n}");
        }
    }

    #[test]
    fn branch_at_any_index_agrees_with_root() {
        for n in 1..=9usize {
            let leaves: Vec<Hash256> = (1..=n as u8).map(h).collect();
            let root = merkle_root(&leaves);
            for (i, leaf) in leaves.iter().enumerate() {
                let branch = branch_at(&leaves, i);
                assert_eq!(root_from_branch_at(leaf, &branch, i), root, "n={n} i={i}");
            }
            assert_eq!(branch_at(&leaves, 0), coinbase_branch(&leaves[1..]));
        }
    }

    #[test]
    fn two_leaf_root_matches_manual() {
        let root = merkle_root(&[h(1), h(2)]);
        assert_eq!(root, hash_pair(&h(1), &h(2)));
        assert_eq!(coinbase_branch(&[h(2)]), vec![h(2)]);
    }
}
