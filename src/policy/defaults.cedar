// Default permissive policies for development (v0.1)
//
// Implements: REQ-POL-001/F-007 (Embedded Default Policy)
//
// WARNING: These policies permit ALL actions on ALL resources.
// DO NOT USE IN PRODUCTION.
//
// For production, create custom policies in a ConfigMap or
// set THOUGHTGATE_POLICIES environment variable.

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
