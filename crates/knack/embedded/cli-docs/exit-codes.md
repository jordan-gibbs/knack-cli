## Exit codes (stable)

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | user error (bad args, not found) |
| 2 | auth error (not logged in / token expired) |
| 3 | network / API error |
| 4 | conflict (publish needs `--force`) |
| 5 | plan limit (free tier hit cap) |
| 64 | invalid input (POSIX EX_USAGE) |
| 70 | internal CLI bug |

Treat any other non-zero exit as code 70.
