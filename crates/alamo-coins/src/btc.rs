//! Bitcoin: a SHA-256d parent chain, solo mined with BIP310 version rolling.

use crate::{Chain, Coin};
use alamo_core::{AddressParams, Algorithm};

/// The header version bits BIP320 sets aside for miners (bits 13 to 28): what a Bitcoin
/// lane lets miners roll under BIP310.
pub const VERSION_ROLLING_MASK: u32 = 0x1fff_e000;

/// Bitcoin (SHA-256d).
#[derive(Clone, Copy, Debug, Default)]
pub struct Bitcoin;

impl Coin for Bitcoin {
    fn symbol(&self) -> &'static str {
        "BTC"
    }

    fn name(&self) -> &'static str {
        "Bitcoin"
    }

    fn algorithm(&self) -> Algorithm {
        Algorithm::Sha256d
    }

    fn aux_chain_id(&self) -> Option<u32> {
        None
    }

    fn address_params(&self, chain: Chain) -> AddressParams {
        match chain {
            Chain::Main => AddressParams {
                p2pkh_prefix: 0,
                p2sh_prefixes: &[5],
                bech32_hrp: Some("bc"),
            },
            Chain::Test => AddressParams {
                p2pkh_prefix: 111,
                p2sh_prefixes: &[196],
                bech32_hrp: Some("tb"),
            },
            Chain::Regtest => AddressParams {
                p2pkh_prefix: 111,
                p2sh_prefixes: &[196],
                bech32_hrp: Some("bcrt"),
            },
        }
    }

    fn template_rules(&self) -> &'static [&'static str] {
        &["segwit"]
    }

    fn coinbase_maturity(&self, _chain: Chain) -> i64 {
        100
    }

    fn version_rolling_mask(&self) -> u32 {
        VERSION_ROLLING_MASK
    }

    /// Bitcoin Core refuses `getblocktemplate` on signet unless the request carries the
    /// `signet` rule, and a signet block also needs the signet solution in its coinbase,
    /// which the pool does not build.
    fn unsupported_network(&self, chain_name: &str) -> Option<&'static str> {
        (chain_name == "signet").then_some(
            "signet is not supported for Bitcoin: its block templates need the 'signet' rule \
             and a signet solution in the coinbase, which Alamo does not provide; use a \
             mainnet, testnet, or regtest node",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alamo_core::address::{payout_script, AddressError};

    fn main() -> AddressParams {
        Bitcoin.address_params(Chain::Main)
    }

    /// The scriptPubKey `address` pays on mainnet, as hex.
    fn script_hex(address: &str) -> Result<String, AddressError> {
        payout_script(address, &main()).map(hex::encode)
    }

    #[test]
    fn mainnet_base58_addresses() {
        // The genesis block's coinbase key, as a P2PKH address (version 0).
        assert_eq!(
            script_hex("1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa").unwrap(),
            "76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac"
        );
        // The Bitcoin wiki's P2SH example (version 5).
        let p2sh = script_hex("3J98t1WpEZ73CNmQviecrnyiWrnqRhWNLy").unwrap();
        assert!(p2sh.starts_with("a914") && p2sh.ends_with("87"), "{p2sh}");
        assert_eq!(p2sh.len(), 46);
        // A Litecoin address is the wrong network.
        assert_eq!(
            script_hex("LTdsVS8VDw6syvfQADdhf2PHAm3rMGJvPX"),
            Err(AddressError::WrongNetwork)
        );
    }

    #[test]
    fn mainnet_segwit_and_taproot_addresses() {
        // BIP350's valid-address vectors: v0 (bech32) and v1 (bech32m).
        assert_eq!(
            script_hex("BC1QW508D6QEJXTDG4Y5R3ZARVARY0C5XW7KV8F3T4").unwrap(),
            "0014751e76e8199196d454941c45d1b3a323f1433bd6"
        );
        assert_eq!(
            script_hex("bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqzk5jj0").unwrap(),
            "512079be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        );
        // BIP86's first receiving address (m/86'/0'/0'/0/0), a real key-path taproot output.
        assert_eq!(
            script_hex("bc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxqkedrcr").unwrap(),
            "5120a60869f0dbcf1dc659c9cecbaf8050135ea9e8cdc487053f1dc6880949dc684c"
        );
        // BIP350's invalid vectors: a v1 program with a bech32 checksum, a v0 program
        // with a bech32m checksum, and a 1-byte v1 program.
        for bad in [
            "bc1p0xlxvlhemja6c4dqv22uapctqupfhlxm9h8z3k2e72q4k9hcz7vqh2y7hd",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kemeawh",
            "bc1pw5dgrnzv",
        ] {
            assert!(script_hex(bad).is_err(), "{bad}");
        }
        // A testnet taproot address is the wrong network on mainnet.
        assert_eq!(
            script_hex("tb1pqqqqp399et2xygdj5xreqhjjvcmzhxw4aywxecjdzew6hylgvsesf3hn0c"),
            Err(AddressError::WrongNetwork)
        );
        assert_eq!(
            payout_script(
                "tb1pqqqqp399et2xygdj5xreqhjjvcmzhxw4aywxecjdzew6hylgvsesf3hn0c",
                &Bitcoin.address_params(Chain::Test)
            )
            .map(hex::encode)
            .unwrap(),
            "5120000000c4a5cad46221b2a187905e5266362b99d5e91c6ce24d165dab93e86433"
        );
    }

    #[test]
    fn regtest_and_testnet_use_their_own_hrps() {
        let program = [0x42u8; 20];
        let regtest = Bitcoin.address_params(Chain::Regtest);
        let test = Bitcoin.address_params(Chain::Test);
        let bcrt = alamo_core::address::encode_segwit("bcrt", 0, &program).unwrap();
        let tb = alamo_core::address::encode_segwit("tb", 0, &program).unwrap();
        assert!(payout_script(&bcrt, &regtest).is_ok());
        assert_eq!(payout_script(&bcrt, &test), Err(AddressError::WrongNetwork));
        assert!(payout_script(&tb, &test).is_ok());
        assert_eq!(payout_script(&tb, &main()), Err(AddressError::WrongNetwork));
    }

    #[test]
    fn rolls_the_bip320_bits_and_refuses_signet() {
        assert_eq!(Bitcoin.version_rolling_mask(), 0x1fff_e000);
        assert_eq!(Bitcoin.version_rolling_mask().count_ones(), 16);
        assert!(Bitcoin.unsupported_network("signet").is_some());
        for chain in ["main", "test", "testnet4", "regtest"] {
            assert!(Bitcoin.unsupported_network(chain).is_none(), "{chain}");
        }
        assert_eq!(Bitcoin.block_version(0x2000_0000), 0x2000_0000);
    }
}
