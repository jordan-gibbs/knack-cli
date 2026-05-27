# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in the Knack CLI, please email
**security@getknack.ai** with the details. Do not open a public GitHub
issue for security reports.

We aim to acknowledge reports within 3 business days. Please give us a
reasonable window to investigate and ship a fix before any public
disclosure.

You can also use GitHub's private vulnerability reporting from this
repository's Security tab.

## Scope

This policy covers the Knack CLI in this repository: the `knack` binary,
the `knack-types` and `knack-backend-github` crates, and the install
scripts.

The Knack Cloud service (api.getknack.ai, the web app at knack.ai) has a
separate disclosure process documented at https://knack.ai/security.

## Supported versions

Only the latest release receives security fixes. Pinning to an old
version is fine, but a vulnerability report against a pre-latest version
will be fixed in the next release line, not backported.
