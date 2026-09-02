mod challenge;
mod healthcheck;

/// Run a public CLI command before application configuration or runtime startup.
/// Returns `None` when the process should continue into the server entry point.
pub fn dispatch() -> Option<i32> {
    dispatch_arguments(std::env::args().skip(1))
}

fn dispatch_arguments<I>(mut arguments: I) -> Option<i32>
where
    I: Iterator<Item = String>,
{
    match arguments.next().as_deref() {
        Some("challenge") => Some(challenge::run(arguments)),
        Some("healthcheck") => Some(healthcheck::run(arguments)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_commands_intercept_server_startup() {
        assert_eq!(dispatch_arguments(std::iter::empty()), None);
        assert_eq!(
            dispatch_arguments(["unrelated".to_string()].into_iter()),
            None
        );
        assert_eq!(
            dispatch_arguments(["challenge".to_string(), "--help".to_string()].into_iter()),
            Some(0)
        );
        assert_eq!(
            dispatch_arguments(["healthcheck".to_string(), "unexpected".to_string()].into_iter()),
            Some(2)
        );
    }
}
