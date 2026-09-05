//! Minimal JSON-RPC 1.0 client for Bitcoin-derived nodes.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// Current network difficulty.
    pub difficulty: f64,
    /// Hash of the best block.
    pub bestblockhash: String,
}

impl RpcClient {
    /// Create a client for the given node.
    pub fn new(
        url: impl Into<String>,
        user: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        Self {
            url: url.into(),
            user: user.into(),
            password: password.into(),
            http: reqwest::Client::new(),
            next_id: Default::default(),
        }
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
}
