# API contract generation

`GET /openapi/v1.json` exposes an incremental OpenAPI 3.1 contract derived from
Rust handlers and response DTOs using `utoipa`. Current coverage includes the
durable control-job lookup, idempotency-operation lookup, and cancellation routes.
The specification describes successful responses as raw models, errors as
`{title,status}`, named string enums, nullable fields, and Unix-millisecond times.
Cookie and Bearer authentication schemes document authentication; event-manager
authorization remains enforced by the backend.

The rest of `web/src/Api.ts` remains the established client, including its manual
extensions. Covered DTOs are imported from `web/src/contracts/BackendContract.ts`.
The generator writes only that file and cannot overwrite the legacy client.
The job `result` field remains arbitrary JSON; consumers must validate its shape.

After changing a covered DTO or route, regenerate locally:

```bash
scripts/bounded-cargo.sh run --bin export-openapi --locked > openapi/v1.json
scripts/bounded-frontend.sh genapi
scripts/bounded-frontend.sh genapi:check
```

The exporter needs neither PostgreSQL nor a running RSCTF instance. Commit the
Rust annotations, specification, and generated types together. The Rust contract
test compares the committed specification with the derived document; the frontend
CI check regenerates types into an isolated temporary directory and detects drift.
The contract version is the API generation (`1`), independent of release versions.

Add coverage incrementally by annotating the real handler, registering it in
`controllers/api_contract.rs`, deriving its schemas, and testing real responses.
Explicitly annotate custom Serde formats, particularly millisecond timestamps.
For authorization changes, retain PostgreSQL-backed negative tests; generated
types cannot establish access control or runtime behavior.
