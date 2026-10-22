# Security Policy

## Supported Versions

Only the latest release version is actively supported with security updates.

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |
| < 0.1   | :x:                |

## Reporting a Vulnerability

If you discover a security vulnerability in Adult Hub Desktop, please report it privately.

- Do **not** open a public GitHub issue.
- Please email details to the project maintainers with reproduction steps and impact assessment.
- We will acknowledge receipt within 48 hours and work with you on a coordinated disclosure timeline.

## Security Practices

- **In-App Updates**: Update manifests are authenticated via detached Ed25519 digital signatures and checked against pinned public keys. Downloaded binaries are verified using SHA-256 digests.
- **Credential Storage**: Access tokens and sensitive server credentials use platform keyring / DPAPI encryption where available.
- **Diagnostics Redaction**: Automated diagnostics log entries sanitize UUIDs, authorization tokens, usernames, server URLs, and media titles before export.
