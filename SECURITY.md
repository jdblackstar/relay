# Security policy

If you find a security issue in relay, please do not open a public issue first.

## Reporting

Email the maintainer at **josh@jdblackstar.com**, or use
[GitHub private vulnerability reporting](https://github.com/jdblackstar/relay/security/advisories/new)
if you prefer. Include:

- reproduction details,
- affected version,
- expected vs actual behavior,
- any suggested remediation.

If you cannot reach the maintainer privately, open a minimal public issue without exploit details and request a private channel.

## Scope

Areas to prioritize:

- installer/download integrity,
- file write safety and rollback guarantees,
- symlink/path traversal behavior,
- unintended data exfiltration between tool directories.
