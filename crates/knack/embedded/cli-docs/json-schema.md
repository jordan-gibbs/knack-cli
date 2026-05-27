## JSON output schema (v1)

All `--json` outputs use this envelope:

    { "$schema": "knack://cli/v1",
      "ok": true,
      "data": <payload> }

Errors:

    { "$schema": "knack://cli/v1",
      "ok": false,
      "error": { "code": "...", "message": "...", "hint": "..." } }

Per-command payloads — Track E.7 generates from clap.

## Stability rules

- **Code**: stable. Match on this.
- **Message**: human-friendly, may change.
- **Hint**: optional, may change.
- New fields may be added to `data`; consumers must ignore unknown fields.
- Breaking changes bump `$schema` to `knack://cli/v2`.
