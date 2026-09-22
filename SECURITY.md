# Security Policy

## Supported versions

Only the latest release on the [releases page](https://github.com/andrewj-t/wincamcfg/releases) receives security fixes. Older releases are not patched; upgrade to the latest version.

## Reporting a vulnerability

Please do not open a public issue for security problems.

Report vulnerabilities privately through [GitHub Security Advisories](https://github.com/andrewj-t/wincamcfg/security/advisories/new). Include the version affected, a description of the problem, and steps to reproduce it if you have them.

You can expect an acknowledgement within a week. Confirmed issues are fixed in a new release and disclosed through a published advisory once the fix is available.

## What is in scope

wincamcfg is a local command-line tool that talks to webcam drivers through DirectShow. It opens no network connections and reads no configuration files. Its only registry access is reading a camera's `Device Parameters` key, which is world-readable. It runs with the privileges of the user who launches it and never asks for elevation; the one privileged operation, `set --restart-device`, refuses to run unless the process is already elevated. Reports about the handling of command-line input, COM/DirectShow interop, the device restart path, the build and release pipeline (including the published SBOMs and attestations), and dependencies are all welcome.

## Verifying releases

Every release ships with build-provenance attestations and SBOMs. See the [release verification](docs/release-verification.md) guide for how to check them with the GitHub CLI before deploying a binary.
