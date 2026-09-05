//! Stratum v1 wire types (line-delimited JSON-RPC).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A request from a miner.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Request {
    /// Request id; `null` for notifications from the miner.
    #[serde(default)]
    pub id: Value,
    /// Method name, e.g. `mining.subscribe`.
    pub method: String,
    /// Positional parameters.
    #[serde(default)]
    pub params: Value,
}

/// A response to a miner request.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Response {
    /// The id of the request being answered.
    pub id: Value,
    /// Result on success, `null` on error.
    pub result: Value,
    /// `null` on success, `[code, message, data]` on error.
    pub error: Option<StratumError>,
}

/// A server-initiated notification such as `mining.notify`.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Notification {
    /// Always `null` for notifications.
    pub id: Value,
    /// Method name.
    pub method: &'static str,
    /// Positional parameters.
    pub params: Value,
}

/// A stratum error, serialized as the conventional `[code, message, data]` triple.
#[derive(Clone, Debug, PartialEq)]
pub struct StratumError {
    /// Error code (20 other, 21 job not found, 22 duplicate, 23 low difficulty,
    /// 24 unauthorized, 25 not subscribed).
    pub code: i32,
    /// Human-readable message.
    pub message: String,
}

impl StratumError {
    /// Generic error.
    pub fn other(message: impl Into<String>) -> Self {
        Self {
            code: 20,
            message: message.into(),
        }
    }

    /// The method is not supported.
    pub fn unknown_method(method: &str) -> Self {
        Self::other(format!("Unknown method: {method}"))
    }
}

impl Serialize for StratumError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (self.code, &self.message, Value::Null).serialize(serializer)
    }
}

impl Response {
    /// A successful response.
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            id,
            result,
            error: None,
        }
    }

    /// An error response.
    pub fn err(id: Value, error: StratumError) -> Self {
        Self {
            id,
            result: Value::Null,
            error: Some(error),
        }
    }
}

impl Notification {
    /// Build a notification.
    pub fn new(method: &'static str, params: Value) -> Self {
        Self {
            id: Value::Null,
            method,
            params,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_subscribe() {
        let line = r#"{"id": 1, "method": "mining.subscribe", "params": ["cgminer/4.9.0"]}"#;
        let req: Request = serde_json::from_str(line).unwrap();
        assert_eq!(req.id, json!(1));
        assert_eq!(req.method, "mining.subscribe");
        assert_eq!(req.params, json!(["cgminer/4.9.0"]));
    }

    #[test]
    fn parses_missing_params_and_id() {
        let req: Request =
            serde_json::from_str(r#"{"method":"mining.extranonce.subscribe"}"#).unwrap();
        assert_eq!(req.id, Value::Null);
        assert_eq!(req.params, Value::Null);
    }

    #[test]
    fn serializes_error_triple() {
        let resp = Response::err(
            json!(7),
            StratumError {
                code: 23,
                message: "Low difficulty share".into(),
            },
        );
        assert_eq!(
            serde_json::to_string(&resp).unwrap(),
            r#"{"id":7,"result":null,"error":[23,"Low difficulty share",null]}"#
        );
    }

    #[test]
    fn serializes_ok_with_null_error() {
        let resp = Response::ok(json!(1), json!(true));
        assert_eq!(
            serde_json::to_string(&resp).unwrap(),
            r#"{"id":1,"result":true,"error":null}"#
        );
    }

    #[test]
    fn serializes_notification() {
        let n = Notification::new("mining.set_difficulty", json!([1024]));
        assert_eq!(
            serde_json::to_string(&n).unwrap(),
            r#"{"id":null,"method":"mining.set_difficulty","params":[1024]}"#
        );
    }
}
