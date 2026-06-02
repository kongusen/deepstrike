# Security Policy

DeepStrike executes tools, routes model output, and can connect to external providers. Please report security issues privately before opening a public issue.

## Supported Versions

Security fixes target the latest released version unless maintainers state otherwise in a release note.

## Reporting a Vulnerability

Please contact the maintainers through a private GitHub security advisory when available. If that is not available, open a minimal GitHub issue asking for a private contact path and do not include exploit details.

Include:

- Affected package or runtime.
- Version or commit SHA.
- Reproduction steps.
- Impact and expected behavior.
- Any relevant logs with secrets removed.

## Scope

Security-sensitive areas include:

- Tool permission and governance bypasses.
- Sandbox or execution-plane escape.
- Credential exposure in logs, replay, or archives.
- Provider request leakage.
- Cross-session memory, knowledge, or sub-agent isolation failures.

## Handling Secrets

Never include API keys, bearer tokens, private URLs, customer data, or full model transcripts that may contain secrets in a public issue or pull request.
