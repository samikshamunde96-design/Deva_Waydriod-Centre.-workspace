//! Typed protocol boundary for remote Waydroid host control.
//!
//! The transport is intentionally separate from this protocol: a production
//! client can use SSH with authenticated OS accounts, while unit tests can use
//! an in-memory transport. No request in this module accepts executable names,
//! shell text, command arrays, or arbitrary host paths.

use serde::{Deserialize, Serialize};

pub const REMOTE_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteOperation {
    Ping,
    Status,
    ListDevices,
    Screenshot,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteRequest {
    pub protocol_version: u32,
    pub request_id: String,
    pub operation: RemoteOperation,
}

impl RemoteRequest {
    pub fn new(request_id: impl Into<String>, operation: RemoteOperation) -> Result<Self, String> {
        let request_id = request_id.into();
        if request_id.is_empty() || request_id.len() > 128 {
            return Err("The remote request ID must contain 1-128 characters.".to_string());
        }

        Ok(Self {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            request_id,
            operation,
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != REMOTE_PROTOCOL_VERSION {
            return Err(format!(
                "Unsupported remote protocol version {}.",
                self.protocol_version
            ));
        }
        if self.request_id.is_empty() || self.request_id.len() > 128 {
            return Err("The remote request ID must contain 1-128 characters.".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteResponse {
    pub protocol_version: u32,
    pub request_id: String,
    pub ok: bool,
    pub result: Option<serde_json::Value>,
    pub error: Option<RemoteError>,
}

impl RemoteResponse {
    pub fn success(
        request_id: impl Into<String>,
        result: serde_json::Value,
    ) -> Result<Self, String> {
        let request_id = request_id.into();
        if request_id.is_empty() || request_id.len() > 128 {
            return Err("The remote request ID must contain 1-128 characters.".to_string());
        }
        Ok(Self {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            request_id,
            ok: true,
            result: Some(result),
            error: None,
        })
    }

    pub fn failure(
        request_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Result<Self, String> {
        let request_id = request_id.into();
        if request_id.is_empty() || request_id.len() > 128 {
            return Err("The remote request ID must contain 1-128 characters.".to_string());
        }
        Ok(Self {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            request_id,
            ok: false,
            result: None,
            error: Some(RemoteError {
                code: code.into(),
                message: message.into(),
            }),
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != REMOTE_PROTOCOL_VERSION {
            return Err(format!(
                "Unsupported remote protocol version {}.",
                self.protocol_version
            ));
        }
        if self.request_id.is_empty() || self.request_id.len() > 128 {
            return Err("The remote response request ID must contain 1-128 characters.".to_string());
        }
        if self.ok && (self.error.is_some() || self.result.is_none()) {
            return Err("A successful remote response requires a result and no error.".to_string());
        }
        if !self.ok && (self.result.is_some() || self.error.is_none()) {
            return Err("A failed remote response requires an error and no result.".to_string());
        }
        Ok(())
    }
}

pub trait RemoteTransport {
    fn execute(&mut self, request: &RemoteRequest) -> Result<RemoteResponse, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockTransport {
        response: RemoteResponse,
        last_request: Option<RemoteRequest>,
    }

    impl RemoteTransport for MockTransport {
        fn execute(&mut self, request: &RemoteRequest) -> Result<RemoteResponse, String> {
            self.last_request = Some(request.clone());
            Ok(self.response.clone())
        }
    }

    #[test]
    fn request_requires_supported_protocol_and_bounded_request_id() {
        let mut request =
            RemoteRequest::new("stage-c-readonly", RemoteOperation::Status).unwrap();
        assert!(request.validate().is_ok());

        request.protocol_version = REMOTE_PROTOCOL_VERSION + 1;
        assert!(request.validate().is_err());

        assert!(RemoteRequest::new("", RemoteOperation::Ping).is_err());
        assert!(RemoteRequest::new("x".repeat(129), RemoteOperation::Ping).is_err());
    }

    #[test]
    fn only_read_only_operations_are_exposed() {
        let serialized = serde_json::to_string(&RemoteOperation::Status).unwrap();
        assert_eq!(serialized, "\"status\"");

        let screenshot = serde_json::to_string(&RemoteOperation::Screenshot).unwrap();
        assert_eq!(screenshot, "\"screenshot\"");
    }

    #[test]
    fn transport_round_trip_preserves_request_and_response() {
        let request =
            RemoteRequest::new("test-001", RemoteOperation::ListDevices).unwrap();
        let response = RemoteResponse::success(
            "test-001",
            serde_json::json!([{"serial": "192.0.2.10:5555", "state": "device"}]),
        )
        .unwrap();
        let mut transport = MockTransport {
            response: response.clone(),
            last_request: None,
        };

        assert_eq!(transport.execute(&request).unwrap(), response);
        assert_eq!(transport.last_request, Some(request));
        assert!(transport.response.validate().is_ok());
    }

    #[test]
    fn response_rejects_success_without_result_or_failure_with_result() {
        assert!(RemoteResponse {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            request_id: "bad-success".to_string(),
            ok: true,
            result: None,
            error: None,
        }
        .validate()
        .is_err());

        assert!(RemoteResponse {
            protocol_version: REMOTE_PROTOCOL_VERSION,
            request_id: "bad-failure".to_string(),
            ok: false,
            result: Some(serde_json::json!({})),
            error: None,
        }
        .validate()
        .is_err());
    }
}
