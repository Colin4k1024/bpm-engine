# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.2.x   | :white_check_mark: |
| 0.1.x   | :white_check_mark: |
| < 0.1   | :x:                |

## Reporting a Vulnerability

If you discover a security vulnerability in bpm-engine, please report it responsibly.

### How to Report

1. **Do NOT open a public GitHub issue** for security vulnerabilities.
2. Send an email to the maintainers (see repository contacts) with:
   - A description of the vulnerability
   - Steps to reproduce
   - Potential impact
   - Suggested fix (if any)

### What to Expect

- **Acknowledgment**: We will acknowledge receipt within 48 hours.
- **Assessment**: We will assess the vulnerability and determine its severity.
- **Fix**: We will work on a fix and coordinate a release timeline.
- **Disclosure**: We will coordinate with you on public disclosure after the fix is released.

### Scope

This policy covers:
- The bpm-engine Rust crate and all workspace crates
- The REST API server
- The Worker SDK
- Official examples and documentation

### Out of Scope

- Vulnerabilities in third-party dependencies (please report to the respective maintainers)
- Issues that require physical access to the server
- Social engineering attacks

## Security Best Practices

When deploying bpm-engine:

1. **Authentication**: The REST API does not include built-in authentication. Use a reverse proxy (nginx, envoy) with appropriate auth middleware.
2. **Network**: Do not expose the engine directly to the internet. Use internal networks and proper firewall rules.
3. **Secrets**: Never commit secrets, API keys, or credentials to the repository.
4. **Dependencies**: Regularly run `cargo audit` to check for known vulnerabilities in dependencies.
5. **Updates**: Keep bpm-engine and its dependencies up to date.

## Security Features

bpm-engine includes several security-relevant features:

- **Lease-based external tasks**: Prevents duplicate task execution
- **Optimistic concurrency control (CAS)**: Prevents race conditions on token state
- **Persistent timers**: No in-memory state that could be lost
- **Crash recovery**: Deterministic recovery from failures
- **Invariant enforcement**: Formal invariants checked in tests

## Contact

For security-related inquiries, please contact the repository maintainers through GitHub.
