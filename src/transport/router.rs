//! MCP method routing.
//!
//! Implements: REQ-CORE-003/F-002 (Method Routing)
//!
//! Routes MCP methods to appropriate handlers:
//! - `tools/*` -> Policy Engine (classification required)
//! - `tasks/*` -> Task Handler (SEP-1686)
//! - `resources/*`, `prompts/*` -> Policy Engine
//! - Unknown methods -> Passthrough to upstream
//!
//! # Routing Table
//!
//! | Method Pattern | Route To | Notes |
//! |----------------|----------|-------|
//! | `tools/call` | Policy Engine | May become task-augmented |
//! | `tools/list` | Policy Engine | May filter based on policy |
//! | `tasks/get` | Task Handler | SEP-1686 |
//! | `tasks/result` | Task Handler | SEP-1686 |
//! | `tasks/list` | Task Handler | SEP-1686 |
//! | `tasks/cancel` | Task Handler | SEP-1686 |
//! | `resources/*` | Policy Engine | Subject to classification |
//! | `prompts/*` | Policy Engine | Subject to classification |
//! | `*` (unknown) | Pass Through | Forward to upstream |

use crate::transport::jsonrpc::McpRequest;

/// Target for routing a request.
///
/// Implements: REQ-CORE-003/§6.4 (Internal: Routing Decision)
///
/// Each variant represents a different handling path for the request.
/// The request is moved into the variant to avoid cloning.
#[derive(Debug)]
pub enum RouteTarget {
    /// Forward to policy engine for classification.
    ///
    /// The policy engine will determine Forward/Approve/Reject action
    /// based on Cedar policies (v0.1 simplified model). Used for tool calls,
    /// resource access, and prompt execution.
    PolicyEvaluation {
        /// The request to evaluate
        request: McpRequest,
    },

    /// Handle internally via task manager (SEP-1686).
    ///
    /// Task methods are handled by ThoughtGate itself, not forwarded
    /// to the upstream MCP server.
    TaskHandler {
        /// The specific task method
        method: TaskMethod,
        /// The request to handle
        request: McpRequest,
    },

    /// Forward directly to upstream (unknown methods).
    ///
    /// Methods that ThoughtGate doesn't recognize are passed through
    /// transparently to the upstream MCP server.
    PassThrough {
        /// The request to forward
        request: McpRequest,
    },

    /// Handle `initialize` method for capability injection.
    ///
    /// Implements: REQ-CORE-007/F-001 (Capability Injection)
    ///
    /// The initialize method is intercepted to:
    /// 1. Forward to upstream and get response
    /// 2. Extract and cache upstream capability detection
    /// 3. Inject ThoughtGate's task capability
    /// 4. Conditionally advertise SSE (only if upstream supports it)
    InitializeHandler {
        /// The initialize request to handle
        request: McpRequest,
    },
}

/// SEP-1686 task method variants.
///
/// These methods are defined by SEP-1686 for task-based async execution.
/// They are handled internally by ThoughtGate's task manager.
///
/// Implements: REQ-CORE-003/§6.4
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskMethod {
    /// `tasks/get` - Get task status and metadata
    Get,
    /// `tasks/result` - Get task result (blocks until complete or timeout)
    Result,
    /// `tasks/list` - List all tasks for the current session
    List,
    /// `tasks/cancel` - Cancel a pending or running task
    Cancel,
}

impl TaskMethod {
    /// Returns the method name as a string.
    ///
    /// Implements: REQ-CORE-003/§6.4 (Internal: Routing Decision)
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskMethod::Get => "tasks/get",
            TaskMethod::Result => "tasks/result",
            TaskMethod::List => "tasks/list",
            TaskMethod::Cancel => "tasks/cancel",
        }
    }
}

/// Router for MCP methods.
///
/// Implements: REQ-CORE-003/F-002 (Method Routing)
///
/// The router is stateless and simply matches method names to route targets.
/// Configuration of routing behavior (e.g., which methods require approval)
/// is handled by the policy engine.
#[derive(Debug, Clone, Default)]
pub struct McpRouter;

impl McpRouter {
    /// Create a new router.
    ///
    /// Implements: REQ-CORE-003/F-002 (Method Routing)
    pub fn new() -> Self {
        Self
    }

    /// Route a request to the appropriate handler.
    ///
    /// Implements: REQ-CORE-003/F-002
    ///
    /// # Arguments
    ///
    /// * `request` - The parsed MCP request to route
    ///
    /// # Returns
    ///
    /// A `RouteTarget` indicating where the request should be handled.
    ///
    /// # Routing Logic
    ///
    /// 1. Check for SEP-1686 task methods (`tasks/*`) -> TaskHandler
    /// 2. Check for policy-controlled methods (`tools/*`, `resources/*`, `prompts/*`) -> PolicyEvaluation
    /// 3. Everything else -> PassThrough
    pub fn route(&self, request: McpRequest) -> RouteTarget {
        let method = request.method.as_str();

        // Initialize method - intercept for capability injection
        // Implements: REQ-CORE-007/F-001 (Capability Injection)
        if method == "initialize" {
            return RouteTarget::InitializeHandler { request };
        }

        // Task methods (SEP-1686) - handle internally
        if let Some(task_method) = Self::parse_task_method(method) {
            return RouteTarget::TaskHandler {
                method: task_method,
                request,
            };
        }

        // Methods subject to policy evaluation
        if method.starts_with("tools/")
            || method.starts_with("resources/")
            || method.starts_with("prompts/")
        {
            return RouteTarget::PolicyEvaluation { request };
        }

        // Everything else passes through to upstream
        RouteTarget::PassThrough { request }
    }

    /// Parse a task method string into TaskMethod enum.
    ///
    /// # Arguments
    ///
    /// * `method` - The method name to parse
    ///
    /// # Returns
    ///
    /// * `Some(TaskMethod)` if the method is a known task method
    /// * `None` otherwise
    fn parse_task_method(method: &str) -> Option<TaskMethod> {
        match method {
            "tasks/get" => Some(TaskMethod::Get),
            "tasks/result" => Some(TaskMethod::Result),
            "tasks/list" => Some(TaskMethod::List),
            "tasks/cancel" => Some(TaskMethod::Cancel),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::jsonrpc::JsonRpcId;
    use std::time::Instant;
    use uuid::Uuid;

    /// Create a test request with the given method.
    fn make_request(method: &str) -> McpRequest {
        McpRequest {
            id: Some(JsonRpcId::Number(1)),
            method: method.to_string(),
            params: None,
            task_metadata: None,
            received_at: Instant::now(),
            correlation_id: Uuid::new_v4(),
        }
    }

    /// Verifies: F-002 (tools/call -> PolicyEvaluation)
    #[test]
    fn test_route_tools_call() {
        let router = McpRouter::new();
        let req = make_request("tools/call");
        let target = router.route(req);
        assert!(matches!(target, RouteTarget::PolicyEvaluation { .. }));
    }

    /// Verifies: F-002 (tools/list -> PolicyEvaluation)
    #[test]
    fn test_route_tools_list() {
        let router = McpRouter::new();
        let req = make_request("tools/list");
        let target = router.route(req);
        assert!(matches!(target, RouteTarget::PolicyEvaluation { .. }));
    }

    /// Verifies: F-002 (tasks/get -> TaskHandler)
    #[test]
    fn test_route_tasks_get() {
        let router = McpRouter::new();
        let req = make_request("tasks/get");
        if let RouteTarget::TaskHandler { method, .. } = router.route(req) {
            assert_eq!(method, TaskMethod::Get);
        } else {
            panic!("Expected TaskHandler");
        }
    }

    /// Verifies: F-002 (tasks/result -> TaskHandler)
    #[test]
    fn test_route_tasks_result() {
        let router = McpRouter::new();
        let req = make_request("tasks/result");
        if let RouteTarget::TaskHandler { method, .. } = router.route(req) {
            assert_eq!(method, TaskMethod::Result);
        } else {
            panic!("Expected TaskHandler");
        }
    }

    /// Verifies: F-002 (tasks/list -> TaskHandler)
    #[test]
    fn test_route_tasks_list() {
        let router = McpRouter::new();
        let req = make_request("tasks/list");
        if let RouteTarget::TaskHandler { method, .. } = router.route(req) {
            assert_eq!(method, TaskMethod::List);
        } else {
            panic!("Expected TaskHandler");
        }
    }

    /// Verifies: F-002 (tasks/cancel -> TaskHandler)
    #[test]
    fn test_route_tasks_cancel() {
        let router = McpRouter::new();
        let req = make_request("tasks/cancel");
        if let RouteTarget::TaskHandler { method, .. } = router.route(req) {
            assert_eq!(method, TaskMethod::Cancel);
        } else {
            panic!("Expected TaskHandler");
        }
    }

    /// Verifies: EC-MCP-007 (Unknown method -> PassThrough)
    #[test]
    fn test_route_unknown_method() {
        let router = McpRouter::new();
        let req = make_request("custom/method");
        let target = router.route(req);
        assert!(matches!(target, RouteTarget::PassThrough { .. }));
    }

    /// Verifies: F-002 (resources/* -> PolicyEvaluation)
    #[test]
    fn test_route_resources() {
        let router = McpRouter::new();
        let req = make_request("resources/list");
        let target = router.route(req);
        assert!(matches!(target, RouteTarget::PolicyEvaluation { .. }));
    }

    /// Verifies: F-002 (resources/read -> PolicyEvaluation)
    #[test]
    fn test_route_resources_read() {
        let router = McpRouter::new();
        let req = make_request("resources/read");
        let target = router.route(req);
        assert!(matches!(target, RouteTarget::PolicyEvaluation { .. }));
    }

    /// Verifies: F-002 (prompts/* -> PolicyEvaluation)
    #[test]
    fn test_route_prompts() {
        let router = McpRouter::new();
        let req = make_request("prompts/list");
        let target = router.route(req);
        assert!(matches!(target, RouteTarget::PolicyEvaluation { .. }));
    }

    /// Verifies: F-002 (prompts/get -> PolicyEvaluation)
    #[test]
    fn test_route_prompts_get() {
        let router = McpRouter::new();
        let req = make_request("prompts/get");
        let target = router.route(req);
        assert!(matches!(target, RouteTarget::PolicyEvaluation { .. }));
    }

    /// Verifies: REQ-CORE-007/F-001 (initialize -> InitializeHandler)
    #[test]
    fn test_route_initialize_handler() {
        // initialize is intercepted for capability injection
        let router = McpRouter::new();
        let req = make_request("initialize");
        let target = router.route(req);
        assert!(matches!(target, RouteTarget::InitializeHandler { .. }));
    }

    #[test]
    fn test_route_ping_passthrough() {
        let router = McpRouter::new();
        let req = make_request("ping");
        let target = router.route(req);
        assert!(matches!(target, RouteTarget::PassThrough { .. }));
    }

    #[test]
    fn test_route_notifications_passthrough() {
        let router = McpRouter::new();
        let req = make_request("notifications/progress");
        let target = router.route(req);
        assert!(matches!(target, RouteTarget::PassThrough { .. }));
    }

    #[test]
    fn test_task_method_as_str() {
        assert_eq!(TaskMethod::Get.as_str(), "tasks/get");
        assert_eq!(TaskMethod::Result.as_str(), "tasks/result");
        assert_eq!(TaskMethod::List.as_str(), "tasks/list");
        assert_eq!(TaskMethod::Cancel.as_str(), "tasks/cancel");
    }

    #[test]
    fn test_request_preserved_in_route_target() {
        let router = McpRouter::new();
        let correlation_id = Uuid::new_v4();
        let req = McpRequest {
            id: Some(JsonRpcId::String("test-id".to_string())),
            method: "tools/call".to_string(),
            params: Some(serde_json::json!({"name": "test"})),
            task_metadata: None,
            received_at: Instant::now(),
            correlation_id,
        };

        if let RouteTarget::PolicyEvaluation { request } = router.route(req) {
            assert_eq!(request.correlation_id, correlation_id);
            assert_eq!(request.method, "tools/call");
            assert_eq!(request.id, Some(JsonRpcId::String("test-id".to_string())));
        } else {
            panic!("Expected PolicyEvaluation");
        }
    }
}
