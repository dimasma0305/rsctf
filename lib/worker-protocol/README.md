# RSCTF worker protocol

This crate is the runtime-independent wire contract between the RSCTF network owner
and trusted workers. The shipped worker agent and execution backend support Linux
Docker workers only. Windows enum/build fields remain protocol-future metadata; they
do not indicate a usable Windows worker runtime. The crate intentionally contains no
Docker, Kubernetes, database, or HTTP implementation.

- TLS 1.3 with mutual authentication is required by the two endpoints using this
  crate. The crate exposes distinct control and data ALPN identifiers.
- Control and connection handshakes use big-endian length-prefixed JSON capped at
  256 KiB.
- Data streams use a small `RSD1` preamble and an 8 KiB typed JSON header, followed
  by one status byte and raw bytes.
- `ValidatedWorkloadSpec` enforces immutable images, bounded resources, named ports,
  and game-mode safety gates. Only stateless Jeopardy services may have replicas.
  Attack/Defense and King-of-the-Hill fields remain in the wire model, but the
  current remote worker runtime accepts Jeopardy workloads only; competitive-mode
  containers stay on the configured local Docker/Kubernetes backend.

The protocol revision is solely a wire-compatibility identifier. It does not version
or otherwise change scoring.
