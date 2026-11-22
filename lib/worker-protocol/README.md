# RSCTF worker protocol

This crate is the runtime-independent wire contract between the RSCTF network owner
and trusted workers. The shipped worker supports Linux Docker Engine and native
Windows Docker Engine in Windows-container mode. The crate intentionally contains no
Docker, Kubernetes, database, or HTTP implementation.

- TLS 1.3 with mutual authentication is required by the two endpoints using this
  crate. The crate exposes distinct control and data ALPN identifiers.
- Control and connection handshakes use big-endian length-prefixed JSON capped at
  256 KiB.
- Data streams use a small `RSD1` preamble and an 8 KiB typed JSON header, followed
  by one status byte and raw bytes.
- `ValidatedWorkloadSpec` enforces immutable images, bounded resources, named ports,
  and game-mode safety gates. Only stateless Jeopardy services may have replicas.
  The platform currently places Jeopardy workloads on trusted workers. Hosted
  A&D and KotH use the configured local Docker or Kubernetes backend.
- A Linux A&D workload can declare an optional `teamDataMount` of `/data` or
  `/var/lib/<application>`. The worker retains a private Docker volume across
  generations of that assignment and releases it on final teardown. A worker
  configured to require disk quotas rejects this mount because the local volume
  driver cannot enforce a per-team size limit. Use this field only with a worker
  advertising the `teamDataMount` capability. Older agents omit the capability.

The protocol revision is solely a wire-compatibility identifier. It does not version
or otherwise change scoring.
