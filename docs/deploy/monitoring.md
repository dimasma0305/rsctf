# Runtime monitoring

Every serving role exposes `GET /metrics` in Prometheus text format alongside
its health endpoints. Set `RSCTF_METRICS_TOKEN` to a randomly generated token of
at least 32 characters and configure the scraper's Bearer authorization header.
Missing, short, or incorrect tokens receive HTTP 401. Keep this route restricted
to the monitoring network at ingress; health probes remain independent.

Scrape each replica directly. Set bounded target labels such as `role`, `instance`,
and `installation` in Prometheus service discovery. Scraping a load balancer hides
individual replicas and makes process counters appear to reset between requests.

The existing `/api/admin/realtime/metrics` JSON contract is preserved. Both
representations use the same fixed-cardinality atomics. Exposition has fewer than
160 series and includes no user IDs, flags, paths, credentials, or cache keys.
The exporter does not query PostgreSQL or allocate a second metrics registry.

- Cache L1/L2 hits and misses are cumulative counters; use `rate()` when combining
  replicas. `cache_read_seconds` and `cache_l2_read_seconds` include misses.
- Checker scheduling delay, execution, and scoreboard rebuild timings are
  cumulative histograms with seconds as their unit. Histogram buckets include
  `+Inf`; `_count` equals that bucket. Use `histogram_quantile()` over the sum of
  bucket rates, grouped by `le` and role, rather than averaging replica quantiles.
- Control-job start, success, and failure counters describe executor attempts.
  Queue delay measures time from durable creation to execution, including prior
  attempts; execution timing includes failed and cancelled futures. A restart
  resets process counters but does not erase durable job history.
- `transaction_acquire_seconds` includes pool waiting and the `BEGIN` handshake
  for the shared cancellation-safe transaction helpers. It does not represent
  every standalone query's checkout time. Pool size and idle connections are
  gauges; compare them per replica with its configured pool budget.

Start with alerts for readiness failure, sustained pool exhaustion, an increasing
checker timeout/no-runway rate, or control-job failures. Choose latency thresholds
from the event's tick budget and measured baseline; a single universal threshold
does not describe all deployments. Monitor process restarts alongside counter rates.

The `metrics`/`metrics-exporter-prometheus` combination is a reasonable option when
dynamic instrumentation or third-party recorder integration is needed. This
exporter reuses the platform's existing histogram buckets and counters to preserve
the admin contract and avoid two recorders on each hot path. That choice is not a
claim of a measured performance advantage over those libraries.
