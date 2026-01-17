// Default permissive policies for development
//
// Implements: REQ-POL-001/F-007 (Embedded Default Policy)
//
// WARNING: These policies permit ALL actions on ALL resources.
// DO NOT USE IN PRODUCTION.
//
// For production, create custom policies in a ConfigMap or
// set THOUGHTGATE_POLICIES environment variable.

// ═══════════════════════════════════════════════════════════
// v0.2 Actions (Permit/Forbid evaluation)
// ═══════════════════════════════════════════════════════════

// Permit all tools/call actions
// In v0.2, Cedar is Gate 3 and only evaluates. Routing is handled by YAML.
permit(
    principal,
    action == ThoughtGate::Action::"tools/call",
    resource
);

// Permit all mcp/method actions
permit(
    principal,
    action == ThoughtGate::Action::"mcp/method",
    resource
);

// ═══════════════════════════════════════════════════════════
// v0.1 Legacy Actions (Deprecated - retained for migration)
// ═══════════════════════════════════════════════════════════

// Permit all Forward actions (send to upstream immediately)
permit(
    principal,
    action == ThoughtGate::Action::"Forward",
    resource
);

// Permit all Approve actions (require human approval)
permit(
    principal,
    action == ThoughtGate::Action::"Approve",
    resource
);
