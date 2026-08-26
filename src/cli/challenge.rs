use std::path::{Path, PathBuf};

use rsctf::services::git_sync::{validate_repository, RepositoryValidationReport};
use rsctf::utils::scoring::{
    DEFAULT_CHALLENGE_SUBMISSION_LIMIT, DEFAULT_JEOPARDY_DIFFICULTY,
    DEFAULT_JEOPARDY_MIN_SCORE_RATE,
};

const SUCCESS: i32 = 0;
const VALIDATION_FAILED: i32 = 1;
const USAGE_ERROR: i32 = 2;

#[derive(Debug, Eq, PartialEq)]
struct CheckOptions {
    root: PathBuf,
    github: bool,
    deny_warnings: bool,
}

enum ChallengeCommand {
    Help,
    Version,
    Check(CheckOptions),
}

fn usage() -> &'static str {
    "Usage: rsctf challenge check [--github] [--deny-warnings] [REPOSITORY]\n\
     \n\
     Validates .gzevent and challenge.yaml files without importing or executing them.\n\
     --github         Emit GitHub Actions workflow annotations\n\
     --deny-warnings  Return failure when warnings are present\n\
     -h, --help       Show this help\n\
     -V, --version    Show the rsctf version"
}

fn parse_check_options<I>(arguments: I) -> Result<ChallengeCommand, String>
where
    I: Iterator<Item = String>,
{
    let mut github = false;
    let mut deny_warnings = false;
    let mut root = None;
    for argument in arguments {
        match argument.as_str() {
            "--github" => github = true,
            "--deny-warnings" => deny_warnings = true,
            "-h" | "--help" => return Ok(ChallengeCommand::Help),
            "-V" | "--version" => return Ok(ChallengeCommand::Version),
            value if value.starts_with('-') => {
                return Err(format!("unknown option {value:?}\n\n{}", usage()));
            }
            value if root.is_none() => root = Some(PathBuf::from(value)),
            value => return Err(format!("unexpected argument {value:?}\n\n{}", usage())),
        }
    }
    Ok(ChallengeCommand::Check(CheckOptions {
        root: root.unwrap_or_else(|| PathBuf::from(".")),
        github,
        deny_warnings,
    }))
}

fn parse_command<I>(mut arguments: I) -> Result<ChallengeCommand, String>
where
    I: Iterator<Item = String>,
{
    match arguments.next().as_deref() {
        Some("check") => parse_check_options(arguments),
        Some("-h" | "--help") => Ok(ChallengeCommand::Help),
        Some(command) => Err(format!(
            "unknown challenge command {command:?}\n\n{}",
            usage()
        )),
        None => Err(format!("missing challenge command\n\n{}", usage())),
    }
}

fn github_escape_data(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn github_escape_property(value: &str) -> String {
    github_escape_data(value)
        .replace(':', "%3A")
        .replace(',', "%2C")
}

fn render(report: &RepositoryValidationReport, root: &Path, github: bool) {
    for diagnostic in &report.diagnostics {
        let path = diagnostic.path.to_string_lossy();
        if github {
            println!(
                "::{} file={}::{}",
                diagnostic.level.as_str(),
                github_escape_property(&path),
                github_escape_data(&diagnostic.message)
            );
        } else {
            println!(
                "{}: {}: {}",
                if path.is_empty() {
                    root.display().to_string()
                } else {
                    path.into_owned()
                },
                diagnostic.level.as_str(),
                diagnostic.message
            );
        }
    }
    println!(
        "checked {} event(s) and {} challenge(s): {} error(s), {} warning(s)",
        report.event_count,
        report.challenge_count,
        report.error_count(),
        report.warning_count()
    );
    if report.is_valid() {
        println!(
            "rsctf defaults: minScoreRate={}, difficulty={}, submissionLimit={}",
            DEFAULT_JEOPARDY_MIN_SCORE_RATE,
            DEFAULT_JEOPARDY_DIFFICULTY,
            DEFAULT_CHALLENGE_SUBMISSION_LIMIT
        );
    }
}

fn run_check(options: CheckOptions) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("failed to initialize the validation runtime: {error}");
            return USAGE_ERROR;
        }
    };
    let report = runtime.block_on(validate_repository(&options.root));
    render(&report, &options.root, options.github);
    if report.is_valid() && !(options.deny_warnings && report.warning_count() > 0) {
        SUCCESS
    } else {
        VALIDATION_FAILED
    }
}

pub fn run<I>(arguments: I) -> i32
where
    I: Iterator<Item = String>,
{
    match parse_command(arguments) {
        Ok(ChallengeCommand::Help) => {
            println!("{}", usage());
            SUCCESS
        }
        Ok(ChallengeCommand::Version) => {
            println!("rsctf {}", env!("CARGO_PKG_VERSION"));
            SUCCESS
        }
        Ok(ChallengeCommand::Check(options)) => run_check(options),
        Err(error) => {
            eprintln!("{error}");
            USAGE_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<ChallengeCommand, String> {
        parse_command(arguments.iter().map(|argument| (*argument).to_string()))
    }

    #[test]
    fn check_options_are_bounded_and_order_independent() {
        let ChallengeCommand::Check(options) =
            parse(&["check", "--deny-warnings", "fixtures", "--github"]).unwrap()
        else {
            panic!("expected check command");
        };
        assert_eq!(
            options,
            CheckOptions {
                root: PathBuf::from("fixtures"),
                github: true,
                deny_warnings: true,
            }
        );
        assert!(parse(&["check", "one", "two"]).is_err());
        assert!(parse(&["check", "--unknown"]).is_err());
    }

    #[test]
    fn challenge_namespace_rejects_unknown_or_missing_commands() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["unknown"]).is_err());
    }

    #[test]
    fn github_escaping_covers_commands_and_properties() {
        assert_eq!(github_escape_data("a%\nb\r"), "a%25%0Ab%0D");
        assert_eq!(github_escape_property("a:b,c"), "a%3Ab%2Cc");
    }
}
