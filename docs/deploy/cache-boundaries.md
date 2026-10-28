# Cache responsibilities

The Redis-backed cache now has separate internal APIs for ordinary responses and
local plaintext secrets. They share a bounded allocation budget of 4,096 entries
and 32 MiB of keys/payloads, split across sixteen independent shards. Eviction
metadata remains bounded by the existing generation/FIFO policy. Hash table and
allocator overhead, and responses still held by callers, are additional memory.

Responses retain immutable `Bytes`; a hit does not copy the payload. Secret keys
use a reserved namespace that the response API cannot read or overwrite. The
secret store has no Redis connection. Invalidations clear both local views.
Responses and secrets still compete for the same capacity; there is no fixed
reservation that lowers the response cache's available capacity when secrets are
absent.

Authoritative version reads, compare-and-set, one-time consumption, and reservation
restoration continue to use the existing L2 implementations. They bypass the
response shards. Redis remains required when configured; an outage does not switch
authority to a local map. The standalone in-memory backend retains its established
atomic implementation and hard 16,384-entry/64-MiB budget.

One response shard admits at most 256 entries and 2 MiB of keys/payloads. Larger
values remain available from L2 and are served to the caller, but are not retained
in L1. Key skew can cause earlier eviction than a single global map. Deployment
tests should watch L2 hit/miss counts and Redis load, especially for large responses
or small hot sets near the capacity boundary.

The component benchmark compares this implementation with the existing map and
Moka 0.12.16. Moka's asynchronous admission/eviction does not provide the same hard
entry and byte bounds or credential semantics; it is evaluated only as a response
cache candidate. The benchmark is not evidence of platform-wide HTTP performance.
