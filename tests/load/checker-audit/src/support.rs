//! The production preparation, flock, PostgreSQL lock, and cancellation-safe
//! transaction code are included unchanged. Only application composition, error
//! construction, unused slugging, and the transaction metric sink are substituted.
pub mod error {
    pub type AppResult<T> = Result<T, AppError>;
    #[derive(Debug)]
    pub struct AppError(String);
    impl std::fmt::Display for AppError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            self.0.fmt(f)
        }
    }
    impl std::error::Error for AppError {}
    impl From<anyhow::Error> for AppError {
        fn from(value: anyhow::Error) -> Self {
            Self(value.to_string())
        }
    }
    impl AppError {
        pub fn internal(value: impl Into<String>) -> Self {
            Self(value.into())
        }
        pub fn bad_request(value: impl Into<String>) -> Self {
            Self(value.into())
        }
    }
}
pub mod app_state {
    pub struct Config {
        pub storage_root: String,
    }
    pub struct SharedState {
        pub config: Config,
        pub pool: sqlx::PgPool,
    }
    impl SharedState {
        pub fn pg(&self) -> &sqlx::PgPool {
            &self.pool
        }
    }
}
pub mod services {
    pub mod observability {
        pub struct Sink;
        pub static TRANSACTION_ACQUIRE: Sink = Sink;
        impl Sink {
            pub fn timer(&self) {}
        }
    }
}
pub mod package {
    pub fn slugify(_: &str) -> String {
        panic!("audit uses explicit immutable paths")
    }
}
pub fn usage(who: libc::c_int) -> (f64, i64) {
    let mut value = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    assert_eq!(unsafe { libc::getrusage(who, value.as_mut_ptr()) }, 0);
    let value = unsafe { value.assume_init() };
    let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1e6;
    (
        seconds(value.ru_utime) + seconds(value.ru_stime),
        value.ru_maxrss,
    )
}
pub fn distribution(mut values: Vec<f64>) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    let p = |percent: f64| values[((values.len() - 1) as f64 * percent).round() as usize];
    serde_json::json!({"avg_ms": values.iter().sum::<f64>() / values.len() as f64,
        "p50_ms": p(0.5), "p90_ms": p(0.9), "p95_ms": p(0.95), "p99_ms": p(0.99), "max_ms": p(1.0)})
}
