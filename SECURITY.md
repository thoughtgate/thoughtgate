# Security Considerations

## Tool Arguments in Slack Approval Messages (H-001)

ThoughtGate sends tool call arguments as **plaintext** in Slack Block Kit
messages when a governance rule triggers human approval. This is by design:
reviewers need to see the full arguments to make informed approval decisions.

### Risk

Tool arguments may contain sensitive data — API keys, credentials, PII, or
other secrets — that will be visible to **all members of the Slack channel**
receiving approval requests.

### Mitigations

1. **Restrict Slack channel access.** Use a private channel with only
   authorized reviewers. Do not post approval requests to public channels.

2. **Configure field redaction.** Use the `redact_fields` option in your
   workflow configuration to mask sensitive argument fields before they reach
   Slack:

   ```yaml
   governance:
     rules:
       - pattern: "tools/*"
         action: approve
         approval:
           destination:
             type: slack
             channel: "#approvals"
           redact_fields:
             - "credentials.api_key"
             - "password"
   ```

3. **Avoid passing secrets as tool arguments.** Where possible, have the
   MCP server resolve secrets from a vault rather than accepting them as
   call parameters.

### Debug Logging

Tool arguments are **never logged** by ThoughtGate at any log level.
The `Debug` impl for `ApprovalRequest` prints `[REDACTED]` in place of
the arguments field.
