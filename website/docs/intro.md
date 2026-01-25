---
sidebar_position: 1
slug: /
---

# Introduction

ThoughtGate is a high-performance Rust sidecar that acts as a governance layer for AI agents. It intercepts MCP (Model Context Protocol) tool calls and enforces human approval workflows without modifying your agent's code.

Unlike framework-specific solutions like LangChain's `interrupt()`, ThoughtGate can govern closed-source vendor agents and doesn't require application code changes.

## What ThoughtGate Does

```
┌─────────────┐     ┌─────────────────────────────────────┐     ┌─────────────┐
│  AI Agent   │────▶│           ThoughtGate               │────▶│  MCP Server │
│  (Claude,   │◀────│  • YAML governance rules            │◀────│  (Tools)    │
│   GPT, etc) │     │  • Cedar policy engine              │     │             │
└─────────────┘     │  • Slack approval workflows         │     └─────────────┘
                    │  • SEP-1686 async tasks             │
                    └─────────────────────────────────────┘
```

When an AI agent attempts to call a tool, ThoughtGate:

1. **Evaluates the request** through a 4-Gate decision model
2. **Routes appropriately** — forward immediately, require approval, or deny
3. **Manages async tasks** using the SEP-1686 protocol for long-running approvals
4. **Maintains audit trails** of all decisions and outcomes

## Key Features

| Feature | Description |
|---------|-------------|
| **YAML Governance Rules** | Simple glob-based routing for quick setup |
| **Cedar Policies** | AWS Cedar engine for complex access control |
| **Async Approvals** | SEP-1686 task-based approval flow (non-blocking) |
| **Slack Integration** | Human-in-the-loop approval via Slack reactions |
| **Low Overhead** | < 2ms p50 latency, < 20MB memory footprint |

## Quick Links

- **New to ThoughtGate?** Start with the [Quickstart](/docs/how-to/quickstart)
- **Want to understand the concepts?** Read [Why ThoughtGate](/docs/explanation/why-thoughtgate)
- **Ready to deploy?** See [Deploy to Kubernetes](/docs/how-to/deploy-kubernetes)
- **Looking up configuration?** Check the [Configuration Reference](/docs/reference/configuration)
