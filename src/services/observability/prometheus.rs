//! Prometheus 0.0.4 exposition of the existing fixed-cardinality atomics.
//! No second recorder or dynamic label registry is needed on instrumentation paths.
use std::fmt::Write;

use super::*;

fn counter(out: &mut String, name: &str, value: u64) {
    writeln!(out, "# TYPE rsctf_{name} counter\nrsctf_{name} {value}").unwrap();
}

fn histogram(out: &mut String, name: &str, value: HistogramSnapshot) {
    writeln!(out, "# TYPE rsctf_{name}_seconds histogram").unwrap();
    let mut cumulative = 0;
    for (bound, count) in value.upper_bounds_micros.iter().zip(value.counts) {
        cumulative += count;
        let bound = if *bound == u64::MAX {
            "+Inf".into()
        } else {
            (*bound as f64 / 1_000_000.0).to_string()
        };
        writeln!(
            out,
            "rsctf_{name}_seconds_bucket{{le=\"{bound}\"}} {cumulative}"
        )
        .unwrap();
    }
    writeln!(
        out,
        "rsctf_{name}_seconds_sum {}\nrsctf_{name}_seconds_count {}",
        value.total_micros as f64 / 1_000_000.0,
        cumulative
    )
    .unwrap();
}

pub(crate) fn render(pool_size: u32, pool_idle: usize) -> String {
    let mut out = String::with_capacity(12 * 1024);
    for (name, value) in [
        ("cache_l1_hits_total", &CACHE_L1_HITS),
        ("cache_l1_misses_total", &CACHE_L1_MISSES),
        ("cache_l2_hits_total", &CACHE_L2_HITS),
        ("cache_l2_misses_total", &CACHE_L2_MISSES),
        ("checker_started_total", &CHECKER_STARTED),
        ("checker_completed_total", &CHECKER_COMPLETED),
        ("checker_timed_out_total", &CHECKER_TIMED_OUT),
        ("checker_no_runway_total", &CHECKER_NO_RUNWAY),
        ("control_job_started_total", &CONTROL_JOB_STARTED),
        ("control_job_failed_total", &CONTROL_JOB_FAILED),
        ("control_job_succeeded_total", &CONTROL_JOB_SUCCEEDED),
    ] {
        counter(&mut out, name, value.load(Relaxed));
    }
    for (name, value) in [
        ("cache_read", &CACHE_READ),
        ("cache_l2_read", &CACHE_L2_READ),
        ("checker_scheduling_delay", &CHECKER_SCHEDULING_DELAY),
        ("checker_execution", &CHECKER_EXECUTION),
        ("scoreboard_rebuild", &SCOREBOARD_REBUILD),
        ("transaction_acquire", &TRANSACTION_ACQUIRE),
        ("control_job_queue_delay", &CONTROL_JOB_QUEUE_DELAY),
        ("control_job_execution", &CONTROL_JOB_EXECUTION),
    ] {
        histogram(&mut out, name, value.snapshot());
    }
    writeln!(
        out,
        "# TYPE rsctf_database_pool_connections gauge\nrsctf_database_pool_connections {pool_size}"
    )
    .unwrap();
    writeln!(
        out,
        "# TYPE rsctf_database_pool_idle gauge\nrsctf_database_pool_idle {pool_idle}"
    )
    .unwrap();
    use crate::services::observability_state as durable;
    for (name, values) in [
        ("control_jobs_queued", &durable::QUEUED),
        ("control_jobs_running", &durable::RUNNING),
        ("control_jobs_expired_leases", &durable::EXPIRED),
        ("control_jobs_oldest_queued_seconds", &durable::OLDEST),
    ] {
        writeln!(out, "# TYPE rsctf_{name} gauge").unwrap();
        for (kind, value) in durable::JOB_KINDS.iter().zip(values) {
            writeln!(
                out,
                "rsctf_{name}{{kind=\"{kind}\"}} {}",
                value.load(Relaxed)
            )
            .unwrap();
        }
    }
    for (name, value) in [
        ("worker_certificates_expired", &durable::WORKER_CERT_EXPIRED),
        (
            "worker_certificates_expiring_7_days",
            &durable::WORKER_CERT_7_DAYS,
        ),
        (
            "worker_certificates_expiring_30_days",
            &durable::WORKER_CERT_30_DAYS,
        ),
        (
            "durable_metrics_sample_timestamp_seconds",
            &durable::SAMPLED_AT,
        ),
    ] {
        writeln!(
            out,
            "# TYPE rsctf_{name} gauge\nrsctf_{name} {}",
            value.load(Relaxed)
        )
        .unwrap();
    }
    counter(
        &mut out,
        "durable_metrics_sample_errors_total",
        durable::SAMPLE_ERRORS.load(Relaxed),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_uses_cumulative_buckets_seconds_and_matching_infinity_count() {
        let value = Histogram::new();
        for micros in [5, 50, 500, 50_000_000] {
            value.observe(Duration::from_micros(micros));
        }
        let mut out = String::new();
        histogram(&mut out, "test", value.snapshot());
        assert!(out.contains("rsctf_test_seconds_bucket{le=\"0.00001\"} 1\n"));
        assert!(out.contains("rsctf_test_seconds_bucket{le=\"0.0001\"} 2\n"));
        assert!(out.contains("rsctf_test_seconds_bucket{le=\"+Inf\"} 4\n"));
        assert!(out.contains("rsctf_test_seconds_count 4\n"));
        assert!(out.contains("rsctf_test_seconds_sum 50.000555\n"));
    }

    #[test]
    fn exporter_has_a_fixed_small_series_budget() {
        let out = render(50, 12);
        assert!(out.len() < 20_000);
        assert!(out.lines().filter(|line| !line.starts_with('#')).count() < 200);
        assert!(out.contains("rsctf_database_pool_connections 50\n"));
    }
}
