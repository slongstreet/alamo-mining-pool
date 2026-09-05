//! Minimal JSON-RPC 1.0 client for Bitcoin-derived nodes.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Errors from talking to a node.
#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    /// Transport-level failure.
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// The node returned a JSON-RPC error object.
    #[error("rpc error {code}: {message}")]
    Node {
        /// Node error code.
        code: i64,
        /// Node error message.
        message: String,
    },
    /// The response could not be decoded into the expected type.
    #[error("decode error: {0}")]
    Decode(#[from] serde_json::Error),
    /// The node answered with neither a result nor an error.
    #[error("empty response")]
    Empty,
}

/// A JSON-RPC client bound to one node.
#[derive(Clone, Debug)]
pub struct RpcClient {
    url: String,
    user: String,
    password: String,
    http: reqwest::Client,
    next_id: std::sync::Arc<AtomicU64>,
}

#[derive(Serialize)]
struct Request<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: &'a [Value],
}

#[derive(Deserialize)]
struct Response {
    result: Option<Value>,
    error: Option<NodeError>,
}

#[derive(Deserialize)]
struct NodeError {
    code: i64,
    message: String,
}

/// Subset of `getblockchaininfo` the pool cares about.
#[derive(Clone, Debug, Deserialize)]
pub struct BlockchainInfo {
    /// Chain name (`"main"`, `"test"`, `"regtest"`).
    pub chain: String,
    /// Current block height.
    pub blocks: u64,
    /// Current network difficulty as the node reports it.
    pub difficulty: f64,
    /// Hash of the best block.
    pub bestblockhash: String,
}

/// Subset of a verbose `getblock` the pool cares about.
#[derive(Clone, Debug, Deserialize)]
pub struct BlockInfo {
    /// Block hash.
    pub hash: String,
    /// Confirmations; `-1` when the block is not on the active chain.
    pub confirmations: i64,
    /// Height.
    pub height: u64,
}

impl RpcClient {
    /// Create a client for the given node.
    pub fn new(
        url: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client builds");
        Self {
            url: url.into(),
            user: user.into(),
            password: password.into(),
            http,
            next_id: Default::default(),
        }
    }

    /// Parse a URL of the form `http://user:pass@host:port` into a client.
    pub fn from_url_with_userinfo(url: &str) -> Option<Self> {
        let (scheme, rest) = url.split_once("://")?;
        let (userinfo, host) = rest.split_once('@')?;
        let (user, pass) = userinfo.split_once(':')?;
        Some(Self::new(format!("{scheme}://{host}"), user, pass))
    }

    /// Call an arbitrary method and decode the result.
    pub async fn call<T: DeserializeOwned>(
        &self,
        method: &str,
        params: &[Value],
    ) -> Result<T, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = Request {
            jsonrpc: "1.0",
            id,
            method,
            params,
        };
        let response: Response = self
            .http
            .post(&self.url)
            .basic_auth(&self.user, Some(&self.password))
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        if let Some(err) = response.error {
            return Err(RpcError::Node {
                code: err.code,
                message: err.message,
            });
        }
        let result = response.result.ok_or(RpcError::Empty)?;
        Ok(serde_json::from_value(result)?)
    }

    /// `getblockchaininfo`.
    pub async fn get_blockchain_info(&self) -> Result<BlockchainInfo, RpcError> {
        self.call("getblockchaininfo", &[]).await
    }

    /// `getbestblockhash`.
    pub async fn get_best_block_hash(&self) -> Result<String, RpcError> {
        self.call("getbestblockhash", &[]).await
    }

    /// `getblockcount`.
    pub async fn get_block_count(&self) -> Result<u64, RpcError> {
        self.call("getblockcount", &[]).await
    }

    /// `getblocktemplate` with the given rules; returns the raw template object.
    pub async fn get_block_template(&self, rules: &[&str]) -> Result<Value, RpcError> {
        self.call("getblocktemplate", &[json!({ "rules": rules })])
            .await
    }

    /// `submitblock`. `Ok(None)` means accepted; `Ok(Some(reason))` means rejected.
    pub async fn submit_block(&self, block_hex: &str) -> Result<Option<String>, RpcError> {
        let result: Value = self
            .call("submitblock", &[json!(block_hex)])
            .await
            .or_else(|err| match err {
                // submitblock returns JSON null on success, which `call` reports as Empty.
                RpcError::Empty => Ok(Value::Null),
                other => Err(other),
            })?;
        Ok(result.as_str().map(str::to_owned))
    }

    /// Verbose `getblock` for confirmation tracking.
    pub async fn get_block_info(&self, hash: &str) -> Result<BlockInfo, RpcError> {
        self.call("getblock", &[json!(hash), json!(1)]).await
    }

    /// Verbose `getblock` with decoded transactions, as a raw value.
    pub async fn get_block_verbose(&self, hash: &str) -> Result<Value, RpcError> {
        self.call("getblock", &[json!(hash), json!(2)]).await
    }
}
