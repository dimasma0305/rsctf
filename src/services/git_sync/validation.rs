//! Offline repository preflight shared by the CLI and repository importer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use chrono::{Duration, Utc};
use serde::de::DeserializeOwned;

use super::build_matrix::{collect_container_builds, RepositoryContainerBuild};
use super::checker::{checker_source_dir, validate_checker_source};
use super::package::{find_dockerfile_context, parse_enum};
use super::{ChallengeYaml, GzEventModel};
use crate::services::game_config::GameConfiguration;
use crate::utils::enums::{
    ChallengeCategory, ChallengeType, ChallengeVariantMode, SolveReceiptMode,
};
use crate::utils::error::AppResult;
use crate::utils::scoring::{
    validate_challenge_scoring, DEFAULT_CHALLENGE_SUBMISSION_LIMIT, DEFAULT_JEOPARDY_DIFFICULTY,
    DEFAULT_JEOPARDY_MIN_SCORE_RATE, DEFAULT_JEOPARDY_ORIGINAL_SCORE,
};

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum RepositoryDiagnosticLevel {
    Error,
    Warning,
}

impl RepositoryDiagnosticLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepositoryDiagnostic {
    pub level: RepositoryDiagnosticLevel,
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Default)]
pub struct RepositoryValidationReport {
    pub event_count: usize,
    pub challenge_count: usize,
    pub container_builds: Vec<RepositoryContainerBuild>,
    pub diagnostics: Vec<RepositoryDiagnostic>,
}

impl RepositoryValidationReport {
    pub fn error_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.level == RepositoryDiagnosticLevel::Error)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.level == RepositoryDiagnosticLevel::Warning)
            .count()
    }

    pub fn is_valid(&self) -> bool {
        self.error_count() == 0
    }

    fn push(
        &mut self,
        level: RepositoryDiagnosticLevel,
        root: &Path,
        path: &Path,
        message: impl Into<String>,
    ) {
        self.diagnostics.push(RepositoryDiagnostic {
            level,
            path: path
                .strip_prefix(root)
                .ok()
                .filter(|relative| !relative.as_os_str().is_empty())
                .unwrap_or(path)
                .to_path_buf(),
            message: message.into(),
        });
    }

    fn error(&mut self, root: &Path, path: &Path, message: impl Into<String>) {
        self.push(RepositoryDiagnosticLevel::Error, root, path, message);
    }

    fn warning(&mut self, root: &Path, path: &Path, message: impl Into<String>) {
        self.push(RepositoryDiagnosticLevel::Warning, root, path, message);
    }

    fn sort(&mut self) {
        self.diagnostics.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(left.level.cmp(&right.level))
                .then(left.message.cmp(&right.message))
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ResolvedChallengeScoring {
    pub min_score_rate: f64,
    pub difficulty: f64,
    pub submission_limit: i32,
}

pub(super) fn resolve_challenge_scoring(
    model: &ChallengeYaml,
) -> AppResult<ResolvedChallengeScoring> {
    let resolved = ResolvedChallengeScoring {
        min_score_rate: model
            .min_score_rate
            .unwrap_or(DEFAULT_JEOPARDY_MIN_SCORE_RATE),
        difficulty: model.difficulty.unwrap_or(DEFAULT_JEOPARDY_DIFFICULTY),
        submission_limit: model
            .submission_limit
            .unwrap_or(DEFAULT_CHALLENGE_SUBMISSION_LIMIT),
    };
    validate_challenge_scoring(
        DEFAULT_JEOPARDY_ORIGINAL_SCORE,
        resolved.min_score_rate,
        resolved.difficulty,
        resolved.submission_limit,
    )?;
    Ok(resolved)
}

fn event_configuration(model: &GzEventModel) -> GameConfiguration {
    let now = Utc::now();
    let ad = model.ad.as_ref();
    GameConfiguration {
        start_time_utc: model.start.unwrap_or(now + Duration::days(1)),
        end_time_utc: model.end.unwrap_or(now + Duration::days(30)),
        freeze_time_utc: None,
        team_member_count_limit: model.team_member_count_limit.unwrap_or(0),
        container_count_limit: model.container_count_limit.unwrap_or(3),
        ad_warmup_seconds: ad.and_then(|section| section.warmup_seconds),
        ad_snapshot_retention_days: ad.and_then(|section| section.snapshot_retention_days),
        ad_tick_seconds: ad.and_then(|section| section.tick_seconds),
        ad_flag_lifetime_ticks: ad.and_then(|section| section.flag_lifetime_ticks),
        ad_reset_cooldown_minutes: ad.and_then(|section| section.reset_cooldown_minutes),
        ad_getflag_window_fraction: ad.and_then(|section| section.getflag_window_fraction),
        ad_min_grace_period_seconds: ad.and_then(|section| section.min_grace_period_seconds),
        ad_epoch_ticks: 8,
        koth_epoch_ticks: 12,
        koth_cycle_ticks: 3,
        koth_champion_cooldown_ticks: 1,
        koth_claim_confirmation_ticks: 2,
    }
}

async fn parse_yaml<T>(
    root: &Path,
    path: &Path,
    kind: &str,
    report: &mut RepositoryValidationReport,
) -> Option<T>
where
    T: DeserializeOwned,
{
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) => {
            report.error(root, path, format!("cannot inspect {kind}: {error}"));
            return None;
        }
    };
    if !metadata.file_type().is_file() {
        report.error(root, path, format!("{kind} must be a regular file"));
        return None;
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        report.error(
            root,
            path,
            format!("{kind} exceeds the {MAX_MANIFEST_BYTES}-byte limit"),
        );
        return None;
    }
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(raw) => raw,
        Err(error) => {
            report.error(root, path, format!("cannot read {kind} as UTF-8: {error}"));
            return None;
        }
    };
    let mut unknown = Vec::new();
    let deserializer = serde_norway::Deserializer::from_str(&raw);
    let parsed = serde_ignored::deserialize(deserializer, |field| {
        unknown.push(field.to_string());
    });
    match parsed {
        Ok(model) => {
            unknown.sort();
            unknown.dedup();
            for field in unknown {
                report.error(root, path, format!("unknown {kind} field: {field}"));
            }
            Some(model)
        }
        Err(error) => {
            report.error(root, path, format!("invalid {kind}: {error}"));
            None
        }
    }
}

fn path_is_safe_relative(value: &str) -> bool {
    let path = Path::new(value);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

async fn validate_attachment(
    root: &Path,
    manifest: &Path,
    model: &ChallengeYaml,
    challenge_type: ChallengeType,
    report: &mut RepositoryValidationReport,
) {
    let package = manifest.parent().unwrap_or(root);
    let authored = model
        .provide
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(value) = authored {
        if !path_is_safe_relative(value) {
            report.error(
                root,
                manifest,
                "provide must be a relative path without parent traversal",
            );
            return;
        }
        let canonical_package = match tokio::fs::canonicalize(package).await {
            Ok(path) => path,
            Err(error) => {
                report.error(
                    root,
                    manifest,
                    format!("cannot resolve challenge package: {error}"),
                );
                return;
            }
        };
        let target = match tokio::fs::canonicalize(package.join(value)).await {
            Ok(path) => path,
            Err(error) => {
                report.error(
                    root,
                    manifest,
                    format!("provide target {value:?} is unavailable: {error}"),
                );
                return;
            }
        };
        if !target.starts_with(&canonical_package) {
            report.error(
                root,
                manifest,
                format!("provide target {value:?} must remain within the challenge package"),
            );
            return;
        }
        let metadata = match tokio::fs::symlink_metadata(&target).await {
            Ok(metadata) => metadata,
            Err(error) => {
                report.error(
                    root,
                    manifest,
                    format!("provide target {value:?} is unavailable: {error}"),
                );
                return;
            }
        };
        if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
            report.error(
                root,
                manifest,
                format!("provide target {value:?} must be a regular file or directory"),
            );
        }
    } else if challenge_type.is_attachment() && !package.join("dist").is_dir() {
        report.warning(
            root,
            manifest,
            "attachment challenge has neither provide nor a dist directory",
        );
    }
}

fn valid_generator_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_provenance(
    root: &Path,
    manifest: &Path,
    model: &ChallengeYaml,
    challenge_type: ChallengeType,
    report: &mut RepositoryValidationReport,
) {
    let package = manifest.parent().unwrap_or(root);
    let generator = package.join("generator").join("Dockerfile");
    let has_local_generator = std::fs::symlink_metadata(generator.parent().unwrap_or(package))
        .is_ok_and(|metadata| metadata.file_type().is_dir())
        && std::fs::symlink_metadata(&generator)
            .is_ok_and(|metadata| metadata.file_type().is_file());
    let has_provenance = model.variant_mode.is_some()
        || model.variant_generator_image.is_some()
        || model.variant_generator_digest.is_some()
        || model.solve_receipt_mode.is_some()
        || model.receipt_verifier_identity.is_some()
        || has_local_generator;
    if challenge_type.uses_ad_engine() && has_provenance {
        report.error(
            root,
            manifest,
            "challenge variants and solve receipts apply only to Jeopardy challenges",
        );
        return;
    }

    match (
        model.variant_generator_image.as_deref(),
        model.variant_generator_digest.as_deref(),
    ) {
        (None, None) => {}
        (Some(image), Some(digest))
            if !image.trim().is_empty()
                && valid_generator_digest(digest)
                && image
                    .strip_suffix(digest)
                    .is_some_and(|prefix| prefix.ends_with('@')) => {}
        _ => report.error(
            root,
            manifest,
            "variantGeneratorImage and variantGeneratorDigest must be one matching immutable image@sha256 pair",
        ),
    }

    if model.variant_mode == Some(ChallengeVariantMode::PerParticipation)
        && !has_local_generator
        && (model.variant_generator_image.is_none() || model.variant_generator_digest.is_none())
    {
        report.error(
            root,
            manifest,
            "PerParticipation requires generator/Dockerfile or a matching immutable generator image and digest",
        );
    }
    if has_local_generator && model.variant_mode != Some(ChallengeVariantMode::PerParticipation) {
        report.warning(
            root,
            manifest,
            "generator/Dockerfile is present but variantMode is not PerParticipation",
        );
    }
    if model
        .solve_receipt_mode
        .unwrap_or(SolveReceiptMode::Disabled)
        != SolveReceiptMode::Disabled
        && !model
            .receipt_verifier_identity
            .as_deref()
            .is_some_and(|identity| (1..=128).contains(&identity.trim().len()))
    {
        report.error(
            root,
            manifest,
            "enabled solve receipts require a 1 to 128 character receiptVerifierIdentity",
        );
    }
}

async fn validate_container(
    root: &Path,
    manifest: &Path,
    model: &ChallengeYaml,
    challenge_type: ChallengeType,
    report: &mut RepositoryValidationReport,
) {
    let package = manifest.parent().unwrap_or(root);
    let container = model.container.as_ref();
    if !challenge_type.is_container() {
        if container.is_some() {
            report.error(
                root,
                manifest,
                "container applies only to container challenge types",
            );
        }
        return;
    }

    let declared_image = container
        .and_then(|section| section.container_image.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if declared_image.is_none() && find_dockerfile_context(package).is_none() {
        report.error(
            root,
            manifest,
            "container challenge requires container.containerImage or src/Dockerfile (falling back to package Dockerfile)",
        );
    }
    if let Some(section) = container {
        if section.memory_limit.is_some_and(|value| value <= 0) {
            report.error(root, manifest, "container.memoryLimit must be positive");
        }
        if section.cpu_count.is_some_and(|value| value <= 0) {
            report.error(root, manifest, "container.cpuCount must be positive");
        }
        if let Some(storage_limit) = section.storage_limit {
            if let Err(error) =
                crate::services::container::validate_storage_limit_value(storage_limit)
            {
                report.error(root, manifest, error.to_string());
            }
        }
        if section
            .expose_port
            .is_some_and(|value| !(1..=65_535).contains(&value))
        {
            report.error(
                root,
                manifest,
                "container.exposePort must be between 1 and 65535",
            );
        }
        if let Some(network_mode) = section.network_mode {
            if let Err(error) = crate::services::container::validate_network_mode_value(
                challenge_type,
                network_mode,
            ) {
                report.error(root, manifest, error.to_string());
            }
        }
        if section.enable_shared_container == Some(true)
            && challenge_type != ChallengeType::StaticContainer
        {
            report.error(
                root,
                manifest,
                "container.enableSharedContainer applies only to StaticContainer",
            );
        }
        if section.enable_traffic_capture == Some(true)
            && challenge_type != ChallengeType::AttackDefense
        {
            report.warning(
                root,
                manifest,
                "container.enableTrafficCapture is effective only for managed AttackDefense",
            );
        }
    }
}

async fn validate_ad_checker(
    root: &Path,
    manifest: &Path,
    model: &ChallengeYaml,
    challenge_type: ChallengeType,
    report: &mut RepositoryValidationReport,
) {
    if !challenge_type.uses_ad_engine() {
        if model.ad.is_some() {
            report.error(
                root,
                manifest,
                "ad applies only to AttackDefense and KingOfTheHill",
            );
        }
        return;
    }
    let ad = model.ad.as_ref();
    if challenge_type == ChallengeType::KingOfTheHill
        && ad.and_then(|section| section.self_hosted) == Some(true)
    {
        report.error(
            root,
            manifest,
            "ad.selfHosted applies only to AttackDefense",
        );
    }
    if ad
        .and_then(|section| section.checker_image.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some_and(|value| !value.contains("{{"))
    {
        report.error(
            root,
            manifest,
            "container-based ad.checkerImage is unsupported; use checker/run.py",
        );
    }
    if model.flags.as_ref().is_some_and(|flags| !flags.is_empty()) {
        report.error(
            root,
            manifest,
            "flags are ignored by A&D/KotH; use the mode-specific flag or control contract",
        );
    }

    let checker = checker_source_dir(&manifest.parent().unwrap_or(root).join("checker"));
    match checker {
        Some(source) => {
            if let Err(error) = validate_checker_source(&source).await {
                report.error(root, manifest, error.to_string());
            }
        }
        None => report.warning(
            root,
            manifest,
            "A&D/KotH challenge has no checker/run.py; official epoch scoring cannot rely on a prepared functional checker",
        ),
    }
}

async fn validate_challenge(
    root: &Path,
    manifest: &Path,
    model: &ChallengeYaml,
    report: &mut RepositoryValidationReport,
) -> Option<String> {
    let name = model
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty());
    if name.is_none() {
        report.error(root, manifest, "challenge.yaml missing non-empty name");
    }
    if model.ignore == Some(true) {
        report.error(
            root,
            manifest,
            "ignore: true prevents repository synchronization",
        );
    }
    let raw_type = model.challenge_type.as_deref().unwrap_or_default();
    let Some(challenge_type) = parse_enum::<ChallengeType>(raw_type) else {
        report.error(
            root,
            manifest,
            format!("unknown challenge type {raw_type:?}"),
        );
        return name.map(str::to_string);
    };
    if let Some(category) = model.category.as_deref() {
        if parse_enum::<ChallengeCategory>(category).is_none() {
            report.error(
                root,
                manifest,
                format!("unknown challenge category {category:?}"),
            );
        }
    }
    if let Err(error) = resolve_challenge_scoring(model) {
        report.error(root, manifest, error.to_string());
    }
    if let Err(error) = super::policy::validate_pending_manifest(model) {
        report.error(root, manifest, error.to_string());
    }

    let flag_template = model
        .container
        .as_ref()
        .and_then(|container| container.flag_template.as_deref())
        .or(model.flag_template.as_deref())
        .map(str::trim)
        .filter(|template| !template.is_empty());
    if challenge_type == ChallengeType::DynamicContainer {
        if let Some(template) = flag_template {
            if let Err(error) = crate::utils::flag_policy::validate_dynamic_template(template) {
                report.error(root, manifest, error.to_string());
            }
        }
    }

    let mut flags = BTreeSet::new();
    for flag in model.flags.as_deref().unwrap_or_default() {
        let flag = flag.trim();
        if flag.is_empty() {
            report.error(root, manifest, "flags must not contain an empty value");
        } else if !flags.insert(flag) {
            report.error(root, manifest, format!("duplicate static flag {flag:?}"));
        }
    }

    validate_attachment(root, manifest, model, challenge_type, report).await;
    validate_container(root, manifest, model, challenge_type, report).await;
    validate_ad_checker(root, manifest, model, challenge_type, report).await;
    validate_provenance(root, manifest, model, challenge_type, report);
    name.map(str::to_string)
}

fn overlapping_event_roots(events: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let mut overlaps = Vec::new();
    for (index, left) in events.iter().enumerate() {
        let left_root = left.parent().unwrap_or_else(|| Path::new(""));
        for right in events.iter().skip(index + 1) {
            let right_root = right.parent().unwrap_or_else(|| Path::new(""));
            if left_root.starts_with(right_root) || right_root.starts_with(left_root) {
                overlaps.push((left.clone(), right.clone()));
            }
        }
    }
    overlaps
}

/// Validate one repository without a database, Docker daemon, or executable
/// preparation. The check is deliberately stricter than import compatibility:
/// unknown fields are errors so misspelled settings cannot be silently ignored.
pub async fn validate_repository(root: &Path) -> RepositoryValidationReport {
    let mut report = RepositoryValidationReport::default();
    match tokio::fs::symlink_metadata(root).await {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => {
            report.error(root, root, "repository path must be a directory");
            return report;
        }
        Err(error) => {
            report.error(root, root, format!("cannot open repository: {error}"));
            return report;
        }
    }

    let events = match super::discovery::discover_events(root).await {
        Ok(events) => events,
        Err(error) => {
            report.error(root, root, error.to_string());
            return report;
        }
    };
    report.event_count = events.len();
    if events.is_empty() {
        report.error(root, root, "repository contains no .gzevent manifest");
    }
    for (left, right) in overlapping_event_roots(&events) {
        report.error(
            root,
            &right,
            format!(
                "nested .gzevent roots are unsupported because they overlap {}",
                left.strip_prefix(root).unwrap_or(&left).display()
            ),
        );
    }

    let mut event_roots = Vec::new();
    let mut event_titles = BTreeMap::<String, PathBuf>::new();
    for event in &events {
        event_roots.push(event.parent().unwrap_or(root).to_path_buf());
        let Some(model) = parse_yaml::<GzEventModel>(root, event, ".gzevent", &mut report).await
        else {
            continue;
        };
        let title = model
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty());
        match title {
            Some(title) => {
                if let Some(first) = event_titles.insert(title.to_string(), event.clone()) {
                    report.warning(
                        root,
                        event,
                        format!(
                            "event title {title:?} is also used by {}",
                            first.strip_prefix(root).unwrap_or(&first).display()
                        ),
                    );
                }
            }
            None => report.error(root, event, ".gzevent missing non-empty title"),
        }
        if let Err(error) = event_configuration(&model).validate() {
            report.error(root, event, error.to_string());
        }
    }

    let challenges = match super::discovery::discover_challenges(root).await {
        Ok(challenges) => challenges,
        Err(error) => {
            report.error(root, root, error.to_string());
            report.sort();
            return report;
        }
    };
    report.challenge_count = challenges.len();
    match collect_container_builds(root, &challenges) {
        Ok(builds) => report.container_builds = builds,
        Err(error) => report.error(root, root, error),
    }
    let mut names_by_event = BTreeMap::<PathBuf, BTreeMap<String, PathBuf>>::new();
    for manifest in &challenges {
        let owners = event_roots
            .iter()
            .filter(|event_root| manifest.starts_with(event_root))
            .collect::<Vec<_>>();
        let owner = match owners.as_slice() {
            [] => {
                report.error(
                    root,
                    manifest,
                    "challenge manifest is not beneath a .gzevent",
                );
                None
            }
            [owner] => Some((*owner).clone()),
            _ => {
                report.error(
                    root,
                    manifest,
                    "challenge manifest belongs to overlapping .gzevent roots",
                );
                owners
                    .into_iter()
                    .max_by_key(|owner| owner.components().count())
                    .cloned()
            }
        };
        let Some(model) =
            parse_yaml::<ChallengeYaml>(root, manifest, "challenge manifest", &mut report).await
        else {
            continue;
        };
        let name = validate_challenge(root, manifest, &model, &mut report).await;
        if let (Some(owner), Some(name)) = (owner, name) {
            let names = names_by_event.entry(owner).or_default();
            if let Some(first) = names.insert(name.clone(), manifest.clone()) {
                report.error(
                    root,
                    manifest,
                    format!(
                        "duplicate challenge name {name:?}; first declared at {}",
                        first.strip_prefix(root).unwrap_or(&first).display()
                    ),
                );
            }
        }
    }

    report.sort();
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "rsctf-repository-validation-{label}-{}",
            uuid::Uuid::new_v4().simple()
        ))
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn cleanup(root: &Path) {
        if let Err(error) = std::fs::remove_dir_all(root) {
            if error.kind() != std::io::ErrorKind::NotFound {
                panic!("cleanup {}: {error}", root.display());
            }
        }
    }

    #[test]
    fn omitted_scoring_fields_resolve_to_platform_defaults() {
        let model: ChallengeYaml = serde_norway::from_str(
            "name: Default scoring\ntype: StaticAttachment\ncategory: Misc\n",
        )
        .unwrap();
        assert_eq!(
            resolve_challenge_scoring(&model).unwrap(),
            ResolvedChallengeScoring {
                min_score_rate: DEFAULT_JEOPARDY_MIN_SCORE_RATE,
                difficulty: DEFAULT_JEOPARDY_DIFFICULTY,
                submission_limit: DEFAULT_CHALLENGE_SUBMISSION_LIMIT,
            }
        );
    }

    #[test]
    fn invalid_authored_scoring_overrides_are_rejected() {
        for manifest in [
            "name: Invalid floor\ntype: StaticAttachment\nminScoreRate: 1.01\n",
            "name: Invalid difficulty\ntype: StaticAttachment\ndifficulty: 0\n",
            "name: Invalid limit\ntype: StaticAttachment\nsubmissionLimit: -1\n",
        ] {
            let model: ChallengeYaml = serde_norway::from_str(manifest).unwrap();
            assert!(resolve_challenge_scoring(&model).is_err(), "{manifest}");
        }
    }

    #[tokio::test]
    async fn valid_minimal_repository_passes() {
        let root = test_root("valid");
        write(
            &root.join(".gzevent"),
            "title: Validation fixture\nhidden: true\n",
        );
        write(
            &root.join("challenges/Misc/example/challenge.yaml"),
            "name: Example\ntype: StaticAttachment\ncategory: Misc\nflags:\n  - rsctf{fixture}\n",
        );
        write(
            &root.join("challenges/Misc/example/dist/readme.txt"),
            "fixture\n",
        );

        let report = validate_repository(&root).await;
        assert!(report.is_valid(), "{:#?}", report.diagnostics);
        assert_eq!(report.event_count, 1);
        assert_eq!(report.challenge_count, 1);
        cleanup(&root);
    }

    #[tokio::test]
    async fn reports_unknown_fields_and_invalid_scoring_together() {
        let root = test_root("invalid-fields");
        write(
            &root.join(".gzevent"),
            "title: Validation fixture\nhiddden: true\n",
        );
        write(
            &root.join("challenge.yaml"),
            "name: Broken\ntype: StaticAttachment\ncategory: Misc\ndificulty: 4\nsubmissionLimit: -1\n",
        );

        let report = validate_repository(&root).await;
        let messages = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert!(messages.iter().any(|message| message.contains("hiddden")));
        assert!(messages.iter().any(|message| message.contains("dificulty")));
        assert!(messages
            .iter()
            .any(|message| message.contains("Submission limit")));
        cleanup(&root);
    }

    #[tokio::test]
    async fn reports_nested_events_and_unscoped_challenges() {
        let root = test_root("scope");
        write(&root.join("one/.gzevent"), "title: One\n");
        write(&root.join("one/nested/.gzevent"), "title: Two\n");
        write(
            &root.join("outside/challenge.yaml"),
            "name: Outside\ntype: StaticAttachment\ncategory: Misc\n",
        );
        write(&root.join("outside/dist/file.txt"), "fixture\n");

        let report = validate_repository(&root).await;
        let messages = report
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>();
        assert!(messages
            .iter()
            .any(|message| message.contains("nested .gzevent")));
        assert!(messages
            .iter()
            .any(|message| message.contains("not beneath a .gzevent")));
        cleanup(&root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_attachment_escape_through_an_intermediate_symlink() {
        use std::os::unix::fs::symlink;

        let root = test_root("attachment-symlink");
        let outside = test_root("attachment-outside");
        write(&root.join(".gzevent"), "title: Validation fixture\n");
        write(
            &root.join("challenge/challenge.yaml"),
            "name: Escaping attachment\ntype: StaticAttachment\nprovide: link/secret.txt\nflags:\n  - rsctf{fixture}\n",
        );
        write(&outside.join("secret.txt"), "outside\n");
        symlink(&outside, root.join("challenge/link")).unwrap();

        let report = validate_repository(&root).await;
        assert!(report.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("must remain within the challenge package")));
        cleanup(&root);
        cleanup(&outside);
    }

    #[tokio::test]
    async fn immutable_generator_image_requires_the_at_digest_separator() {
        let root = test_root("generator-digest-separator");
        let digest = format!("sha256:{}", "a".repeat(64));
        write(&root.join(".gzevent"), "title: Validation fixture\n");
        write(
            &root.join("challenge/challenge.yaml"),
            &format!(
                "name: Broken generator\ntype: StaticAttachment\nprovide: dist\nvariantMode: PerParticipation\nvariantGeneratorImage: registry.example/generator{digest}\nvariantGeneratorDigest: {digest}\n"
            ),
        );
        write(&root.join("challenge/dist/readme.txt"), "fixture\n");

        let report = validate_repository(&root).await;
        assert!(report.diagnostics.iter().any(|diagnostic| diagnostic
            .message
            .contains("matching immutable image@sha256 pair")));
        cleanup(&root);
    }
}
