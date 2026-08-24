//! JSON-RPC 2.0 types.
//!
//! One message model for both surfaces: MCP over the actor socket and the
//! operator API over the admin socket speak the same framing and the same
//! envelope, so there is one codec in the codebase rather than two (D19).

use serde::{Deserialize, Serialize};

/// A request or notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    /// Absent for a notification, which expects no response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

impl Request {
    pub fn new(id: Id, method: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id: Some(id),
            method: method.into(),
            params: Some(params),
        }
    }

    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Whether the envelope is well formed.
    ///
    /// An actor is untrusted input, so the version is checked rather than
    /// assumed.
    pub fn validate(&self) -> Result<(), Error> {
        if self.jsonrpc != "2.0" {
            return Err(Error::invalid_request("jsonrpc must be \"2.0\""));
        }
        if self.method.is_empty() {
            return Err(Error::invalid_request("method must not be empty"));
        }
        Ok(())
    }

    /// Deserialises the parameters into `T`.
    pub fn parse_params<T: serde::de::DeserializeOwned>(&self) -> Result<T, Error> {
        let value = self
            .params
            .clone()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        serde_json::from_value(value).map_err(|error| Error::invalid_params(format!("{error}")))
    }
}

/// A JSON-RPC identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    Text(String),
}

/// A response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Option<Id>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Error>,
}

impl Response {
    pub fn success(id: Option<Id>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: Option<Id>, error: Error) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

/// A JSON-RPC error.
///
/// `data` carries the structured policy reasons for a denial, so an agent
/// receives what was denied, which constraint denied it, and what the narrower
/// or escalated alternative is — rather than a bare message it can only loop on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Standard JSON-RPC codes.
pub mod codes {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
    /// Application code: the request was refused by policy.
    pub const POLICY_DENIED: i32 = -32000;
    /// Application code: authentication failed. Deliberately one code for
    /// unknown, expired, and revoked tokens alike.
    pub const UNAUTHENTICATED: i32 = -32001;
    /// Application code: the operation requires human approval.
    pub const APPROVAL_REQUIRED: i32 = -32002;
}

impl Error {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::new(codes::PARSE_ERROR, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_REQUEST, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            codes::METHOD_NOT_FOUND,
            format!("{method} is not a method this socket serves"),
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(codes::INTERNAL_ERROR, message)
    }

    /// The single authentication failure.
    ///
    /// Unknown, expired, and revoked tokens are rejected identically, with no
    /// information about which (Phase 1 deliverable 4).
    pub fn unauthenticated() -> Self {
        Self::new(
            codes::UNAUTHENTICATED,
            "no valid session token was presented",
        )
    }

    pub fn policy_denied(message: impl Into<String>) -> Self {
        Self::new(codes::POLICY_DENIED, message)
    }

    pub fn approval_required(message: impl Into<String>) -> Self {
        Self::new(codes::APPROVAL_REQUIRED, message)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;

    #[test]
    fn a_well_formed_request_validates_and_parses_its_params() {
        let request = Request::new(
            Id::Number(1),
            "run_task",
            serde_json::json!({"task": "rust.check"}),
        );
        assert!(request.validate().is_ok());
        #[derive(serde::Deserialize)]
        struct Params {
            task: String,
        }
        let params: Params = request.parse_params().unwrap();
        assert_eq!(params.task, "rust.check");
    }

    #[test]
    fn a_wrong_version_or_empty_method_is_refused() {
        let mut request = Request::new(Id::Number(1), "x", serde_json::json!({}));
        request.jsonrpc = "1.0".to_owned();
        assert!(request.validate().is_err());
        let mut request = Request::new(Id::Number(1), "", serde_json::json!({}));
        request.jsonrpc = "2.0".to_owned();
        assert!(request.validate().is_err());
    }

    #[test]
    fn missing_params_parse_as_an_empty_object() {
        let request = Request {
            jsonrpc: "2.0".to_owned(),
            id: Some(Id::Number(1)),
            method: "mission_status".to_owned(),
            params: None,
        };
        #[derive(serde::Deserialize, Default)]
        #[serde(default)]
        struct Params {
            verbose: bool,
        }
        let params: Params = request.parse_params().unwrap();
        assert!(!params.verbose);
    }

    #[test]
    fn notifications_have_no_identifier() {
        let notification = Request {
            jsonrpc: "2.0".to_owned(),
            id: None,
            method: "notifications/initialized".to_owned(),
            params: None,
        };
        assert!(notification.is_notification());
        assert!(notification.validate().is_ok());
    }

    #[test]
    fn identifiers_round_trip_in_both_forms() {
        for id in [Id::Number(7), Id::Text("abc".to_owned())] {
            let json = serde_json::to_string(&id).unwrap();
            let back: Id = serde_json::from_str(&json).unwrap();
            assert_eq!(id, back);
        }
    }

    #[test]
    fn authentication_failures_carry_no_detail() {
        let error = Error::unauthenticated();
        assert_eq!(error.code, codes::UNAUTHENTICATED);
        assert!(error.data.is_none());
        assert!(
            !error.message.contains("expired") && !error.message.contains("revoked"),
            "the caller must not be able to tell which: {}",
            error.message
        );
    }

    #[test]
    fn denials_can_carry_structured_reasons() {
        let error = Error::policy_denied("rust.check is not in this lease's task scope").with_data(
            serde_json::json!({
                "reasons": [{"reason": "task_not_in_lease", "task": "rust.check"}],
                "alternatives": ["call request_escalation"]
            }),
        );
        let encoded = serde_json::to_value(&error).unwrap();
        assert!(encoded["data"]["alternatives"][0].is_string());
    }

    #[test]
    fn responses_serialise_without_null_placeholders() {
        let success = Response::success(Some(Id::Number(1)), serde_json::json!({"ok": true}));
        let encoded = serde_json::to_string(&success).unwrap();
        assert!(!encoded.contains("\"error\""));
        assert!(!success.is_error());

        let failure = Response::failure(Some(Id::Number(1)), Error::internal("x"));
        let encoded = serde_json::to_string(&failure).unwrap();
        assert!(!encoded.contains("\"result\""));
        assert!(failure.is_error());
    }
}
