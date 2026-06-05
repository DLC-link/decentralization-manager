# Security Policy

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues,
discussions, or pull requests.**

Instead, use one of the following private channels:

- **GitHub private vulnerability reporting** (preferred): open the repository's
  **Security** tab and choose **Report a vulnerability**.
- **Email:** `security@bitsafe.finance`

Please include as much of the following as you can:

- A description of the vulnerability and its potential impact.
- Steps to reproduce, or a proof-of-concept.
- Affected version(s) or commit SHA, and the deployment context
  (binary / Docker / Kubernetes; devnet / testnet / mainnet).
- Any suggested remediation, if you have one.

## What to expect

- We will acknowledge your report as soon as we can.
- We will investigate and keep you informed of progress.
- We will credit reporters in the release notes unless you prefer to remain
  anonymous.

Please give us a reasonable opportunity to address the issue before any public
disclosure.

## Supported versions

This project is under active development. Security fixes are applied to the
latest release and the `main` branch.

## Audits

The governance Daml contracts have undergone third-party security review
(Quantstamp). See [`docs/audit-acknowledgements.md`](audit-acknowledgements.md)
for design decisions made in response to audit findings.
