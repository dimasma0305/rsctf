//! git_sync::push_back — regenerate a `challenge.yml` from a persisted
//! `GameChallenge` row and git-push it back to the binding repo.
//!
//! Ports RSCTF `ChallengeYamlSerializer.Serialize` (the inverse of the parent
//! module's `import_manifest`) plus `GitRepoSyncService.CommitAndPushCoreAsync`,
//! driven by `EditController.TryPushBackAsync`. The git plumbing (`run_git`,
//! [`GitCredentials`](super::GitCredentials)) lives in the parent module and is
//! reused here via `super::`.

use std::path::Path;

use serde::Serialize;

use super::GitCredentials;
use crate::models::data::game_challenge;
use crate::utils::enums::NetworkMode;
use crate::utils::error::{AppError, AppResult};

/// Serializable mirror of [`ChallengeYaml`](super::ChallengeYaml) used to
/// REGENERATE a `challenge.yml` from the DB row (the inverse of
/// [`import_manifest`](super::import_manifest)). A separate struct so the read
/// model stays Deserialize-only; every field is `skip_serializing_if` so a
/// default/absent value is omitted rather than written as a noisy `key:` line —
/// matching YamlDotNet's `OmitNull | OmitEmptyCollections` in RSCTF.
#[derive(Debug, Default, Serialize)]
struct ChallengeYamlOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    challenge_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(rename = "flagTemplate", skip_serializing_if = "Option::is_none")]
    flag_template: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hints: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flags: Option<Vec<String>>,
    /// Source-only attachment path. This cannot be reconstructed from the
    /// attachment row, so push-on-edit carries it forward from the owned yaml.
    #[serde(skip_serializing_if = "Option::is_none")]
    provide: Option<String>,
    #[serde(rename = "minScoreRate", skip_serializing_if = "Option::is_none")]
    min_score_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    difficulty: Option<f64>,
    #[serde(rename = "submissionLimit", skip_serializing_if = "Option::is_none")]
    submission_limit: Option<i32>,
    #[serde(rename = "disableBloodBonus", skip_serializing_if = "Option::is_none")]
    disable_blood_bonus: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    container: Option<ContainerOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ad: Option<AdOut>,
}

/// Serializable mirror of [`ContainerSection`](super::ContainerSection) (write
/// side of the `container:` block).
#[derive(Debug, Default, Serialize)]
struct ContainerOut {
    #[serde(rename = "containerImage", skip_serializing_if = "Option::is_none")]
    container_image: Option<String>,
    #[serde(rename = "memoryLimit", skip_serializing_if = "Option::is_none")]
    memory_limit: Option<i32>,
    #[serde(rename = "cpuCount", skip_serializing_if = "Option::is_none")]
    cpu_count: Option<i32>,
    #[serde(rename = "storageLimit", skip_serializing_if = "Option::is_none")]
    storage_limit: Option<i32>,
    #[serde(rename = "exposePort", skip_serializing_if = "Option::is_none")]
    expose_port: Option<i32>,
    #[serde(
        rename = "enableTrafficCapture",
        skip_serializing_if = "Option::is_none"
    )]
    enable_traffic_capture: Option<bool>,
    #[serde(
        rename = "enableSharedContainer",
        skip_serializing_if = "Option::is_none"
    )]
    enable_shared_container: Option<bool>,
    #[serde(rename = "networkMode", skip_serializing_if = "Option::is_none")]
    network_mode: Option<NetworkMode>,
    #[serde(rename = "flagTemplate", skip_serializing_if = "Option::is_none")]
    flag_template: Option<String>,
}

/// Serializable mirror of [`AdSection`](super::AdSection) (write side of the
/// `ad:` block).
#[derive(Debug, Default, Serialize)]
struct AdOut {
    #[serde(rename = "checkerImage", skip_serializing_if = "Option::is_none")]
    checker_image: Option<String>,
    #[serde(rename = "allowEgress", skip_serializing_if = "Option::is_none")]
    allow_egress: Option<bool>,
    #[serde(rename = "allowSelfReset", skip_serializing_if = "Option::is_none")]
    allow_self_reset: Option<bool>,
    #[serde(rename = "sshRequiresFlag", skip_serializing_if = "Option::is_none")]
    ssh_requires_flag: Option<bool>,
    #[serde(rename = "selfHosted", skip_serializing_if = "Option::is_none")]
    self_hosted: Option<bool>,
}

/// Regenerate a `challenge.yml` string from a persisted [`game_challenge::Model`]
/// plus its flag literals — the inverse of
/// [`import_manifest`](super::import_manifest)'s field mapping, mirroring RSCTF
/// `ChallengeYamlSerializer.Serialize`.
///
/// Round-trip-safe: only NON-default fields are emitted (defaults match the
/// importer — `minScoreRate 0.25`, `difficulty 5`, egress false, self-reset true), so
/// a freshly-imported challenge serializes back to a minimal file with no diff
/// churn. Platform-managed state (build status/log, original score, archive path)
/// is never written. An AUTO-BUILT image tag (`rsctf/<game>/…`, see the parent
/// module's `image_tag`) is omitted so the "build from ./src" intent
/// round-trips instead of freezing into a "pull this registry image" on re-sync.
/// The importer's `Author: **X**\n\n` content prefix is reversed back into a
/// dedicated `author:` field.
pub fn serialize_challenge(ch: &game_challenge::Model, flag_texts: &[String]) -> AppResult<String> {
    serialize_challenge_inner(ch, flag_texts, None)
}

/// Regenerate a manifest while retaining supported fields that are owned only
/// by repository source. The existing manifest is parsed before any overwrite;
/// malformed yaml therefore fails closed instead of silently dropping its
/// attachment `provide:` path.
pub(crate) fn serialize_challenge_preserving_source(
    ch: &game_challenge::Model,
    flag_texts: &[String],
    source_yaml: &str,
) -> AppResult<String> {
    let source = serde_norway::from_str::<super::ChallengeYaml>(source_yaml).map_err(|error| {
        AppError::bad_request(format!(
            "push-back: current challenge manifest is invalid: {error}"
        ))
    })?;
    serialize_challenge_inner(ch, flag_texts, source.provide)
}

fn serialize_challenge_inner(
    ch: &game_challenge::Model,
    flag_texts: &[String],
    provide: Option<String>,
) -> AppResult<String> {
    let (description, author) = strip_author_prefix(&ch.content);
    let hints = ch
        .hints
        .as_ref()
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .filter(|l| !l.is_empty());

    let mut out = ChallengeYamlOut {
        name: Some(ch.title.clone()),
        author,
        description: Some(description),
        challenge_type: Some(format!("{:?}", ch.challenge_type)),
        category: Some(format!("{:?}", ch.category)),
        flag_template: ch.flag_template.clone().filter(|s| !s.is_empty()),
        hints,
        flags: if flag_texts.is_empty() {
            None
        } else {
            Some(flag_texts.to_vec())
        },
        provide: provide
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        min_score_rate: (ch.min_score_rate != 0.25).then_some(ch.min_score_rate),
        difficulty: (ch.difficulty != 5.0).then_some(ch.difficulty),
        submission_limit: (ch.submission_limit != 0).then_some(ch.submission_limit),
        disable_blood_bonus: ch.disable_blood_bonus.then_some(true),
        container: None,
        ad: None,
    };

    if ch.challenge_type.is_container() {
        out.container = Some(ContainerOut {
            container_image: ch
                .container_image
                .clone()
                .filter(|s| !s.is_empty() && !is_auto_built_tag(s, ch.game_id)),
            memory_limit: ch.memory_limit,
            cpu_count: ch.cpu_count,
            storage_limit: ch.storage_limit,
            expose_port: ch.expose_port,
            enable_traffic_capture: ch.enable_traffic_capture.then_some(true),
            enable_shared_container: ch.enable_shared_container.then_some(true),
            network_mode: ch.network_mode.filter(|mode| *mode != NetworkMode::Open),
            flag_template: ch.flag_template.clone().filter(|s| !s.is_empty()),
        });
    }

    if ch.challenge_type.uses_ad_engine() {
        let ad = AdOut {
            checker_image: ch.ad_checker_image.clone().filter(|s| {
                !s.is_empty()
                    && !is_auto_built_tag(s, ch.game_id)
                    && !is_managed_checker_revision(s, ch.game_id)
            }),
            // Egress is deny-by-default; emit only the explicit opt-in.
            allow_egress: ch.ad_allow_egress.then_some(true),
            allow_self_reset: (!ch.ad_allow_self_reset).then_some(false),
            ssh_requires_flag: ch.ad_ssh_requires_flag.then_some(true),
            self_hosted: ch.ad_self_hosted.then_some(true),
        };
        // Skip the block entirely when nothing differs from the defaults.
        if ad.checker_image.is_some()
            || ad.allow_egress.is_some()
            || ad.allow_self_reset.is_some()
            || ad.ssh_requires_flag.is_some()
            || ad.self_hosted.is_some()
        {
            out.ad = Some(ad);
        }
    }

    serde_norway::to_string(&out)
        .map_err(|e| AppError::internal(format!("git_sync: serialize challenge yaml: {e}")))
}

/// True for a platform AUTO-BUILT image tag (`rsctf/<game>/<slug>:latest`, minted by
/// the parent module's `image_tag` on import). Such a tag is not
/// authored source and must never be serialized back into the pushed yaml — doing
/// so would flip a "build me" challenge into a "pull this registry image" on the
/// next sync. (RSCTF's equivalent keys off `rsctf-auto/`; this port's convention
/// is `rsctf/`.)
fn is_auto_built_tag(image: &str, game_id: i32) -> bool {
    let prefix = format!("rsctf/{game_id}/");
    let Some(slug) = image
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix(":latest"))
    else {
        return false;
    };
    !slug.is_empty()
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Prepared process-checker directories are publication state, not authored
/// manifest input. Re-emitting an absolute shared-storage path as checkerImage
/// would make the next replica reject or try to reuse another revision.
fn is_managed_checker_revision(path: &str, game_id: i32) -> bool {
    let components: Vec<_> = std::path::Path::new(path)
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect();
    let Some(suffix) = components.get(components.len().saturating_sub(5)..) else {
        return false;
    };
    suffix.len() == 5
        && suffix[0] == "checkers"
        && suffix[1] == game_id.to_string()
        && !suffix[2].is_empty()
        && suffix[3] == "revisions"
        && suffix[4].len() == 32
        && suffix[4].bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Reverse [`import_manifest`](super::import_manifest)'s author folding: it
/// prepends `Author: **X**\n\n` to the content when the yaml named an author.
/// Split that back out so a re-export gets a clean `author:` field instead of
/// doubling the prefix. Only the exact shape the importer writes is matched
/// (conservative), matching RSCTF `ChallengeYamlSerializer.StripAuthorPrefix`.
fn strip_author_prefix(content: &str) -> (String, Option<String>) {
    const PREFIX: &str = "Author: **";
    if let Some(rest) = content.strip_prefix(PREFIX) {
        if let Some(end) = rest.find("**\n\n") {
            return (rest[end + 4..].to_string(), Some(rest[..end].to_string()));
        }
    }
    (content.to_string(), None)
}

/// Commit and push a single already-written file in the checkout at `dest` to its
/// upstream branch, using `token` for auth. Ports RSCTF
/// `GitRepoSyncService.CommitAndPushCoreAsync`.
///
/// `rel_path` is the file's path RELATIVE to the checkout root (what `git add` and
/// the commit record). `repo_url` is the plain `https://…` origin; the token is
/// embedded as Basic-auth userinfo (see [`GitCredentials::apply`]) only for the
/// push and scrubbed from any error text.
///
/// Behavior faithful to upstream: a per-repo commit identity is set (never global
/// config), only the explicit path is staged (no wholesale `git add .`), an empty
/// staged diff short-circuits to a no-op (no empty commits), and the push targets
/// the checkout's ACTUAL attached branch — a detached (tag/SHA) checkout is
/// refused with a clear error rather than creating a phantom branch. The caller
/// must have run [`sync_repo`](super::sync_repo) first so the checkout exists at
/// HEAD.
pub async fn push_file(
    dest: &Path,
    rel_path: &str,
    repo_url: &str,
    token: &str,
    message: &str,
) -> AppResult<()> {
    let repo_url = super::validate_binding_repo_url(repo_url)?;
    if token.is_empty() {
        return Err(AppError::internal("git_sync: push requires an auth token"));
    }
    if !dest.join(".git").exists() {
        return Err(AppError::internal(format!(
            "git_sync: checkout {} not cloned yet; sync before push",
            dest.display()
        )));
    }

    // Per-repo commit identity — avoid mutating the container's global git config.
    super::run_git(dest, &["config", "user.name", "rsctf admin"]).await?;
    super::run_git(dest, &["config", "user.email", "noreply@rsctf.local"]).await?;
    // Authenticated URL for this one push (token embedded; scrubbed from errors).
    // Pass it directly instead of persisting the PAT in `.git/config`.
    let auth_url = GitCredentials::new(token.to_string()).apply(&repo_url);

    // Stage ONLY the explicit path — never a wholesale add that could sweep up
    // build artifacts a prior import wrote. "--" so a '-'-leading path isn't an opt.
    super::run_git(dest, &["add", "--", rel_path]).await?;

    // No-op detection: an "edit" that wrote back identical yaml stages nothing —
    // skip the commit+push so history doesn't accrue empty commits.
    let staged = super::run_git(dest, &["diff", "--cached", "--name-only"]).await?;
    if staged.trim().is_empty() {
        tracing::info!(dir = %dest.display(), "git_sync: push_file — no changes staged; skip push");
        return Ok(());
    }

    super::run_git(dest, &["commit", "-m", message]).await?;

    // Push to the checkout's actual branch. `sync_repo` checks out a branch ref
    // attached; a tag/SHA ref lands DETACHED — refuse it rather than push to a
    // phantom branch (mirrors RSCTF's detached-HEAD guard).
    let dest_ref = super::run_git(dest, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    let dest_ref = dest_ref.trim();
    if dest_ref.is_empty() || dest_ref == "HEAD" {
        return Err(AppError::internal(
            "git_sync: cannot push a detached checkout (tag/SHA ref); pin the binding to a branch",
        ));
    }
    let refspec = format!("HEAD:refs/heads/{dest_ref}");
    super::git::run_git_network(dest, &auth_url, &["push", "--", &auth_url, &refspec]).await?;
    tracing::info!(dir = %dest.display(), rel = %rel_path, branch = %dest_ref, "git_sync: pushed file");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        is_auto_built_tag, is_managed_checker_revision, serialize_challenge,
        serialize_challenge_preserving_source,
    };
    use crate::models::data::game_challenge;
    use crate::services::git_sync::ChallengeYaml;
    use crate::utils::enums::{
        ChallengeBuildStatus, ChallengeCategory, ChallengeReviewStatus, ChallengeType, NetworkMode,
        ScoreCurve,
    };

    fn challenge(challenge_type: ChallengeType) -> game_challenge::Model {
        game_challenge::Model {
            id: 7,
            game_id: 42,
            title: "Example Service".to_string(),
            content: "Description".to_string(),
            category: ChallengeCategory::Pwn,
            challenge_type,
            hints: None,
            is_enabled: false,
            deadline_utc: None,
            submission_limit: 0,
            accepted_count: 0,
            submission_count: 0,
            container_image: None,
            memory_limit: None,
            storage_limit: None,
            cpu_count: None,
            expose_port: None,
            workload_spec: None,
            file_name: None,
            flag_template: None,
            review_status: ChallengeReviewStatus::Active,
            review_note: None,
            submitted_by_user_id: None,
            submitted_at_utc: None,
            reviewed_at_utc: None,
            original_archive_blob_path: None,
            build_context_subdir: None,
            build_status: ChallengeBuildStatus::None,
            build_image_digest: None,
            last_build_log: None,
            source_yaml_path: None,
            attachment_id: None,
            test_container_id: None,
            enable_traffic_capture: false,
            enable_shared_container: false,
            disable_blood_bonus: false,
            original_score: 1000,
            min_score_rate: 0.25,
            difficulty: 5.0,
            score_curve: ScoreCurve::Standard,
            shared_container_id: None,
            network_mode: Some(NetworkMode::Open),
            ad_checker_image: None,
            ad_allow_egress: false,
            ad_allow_self_reset: true,
            ad_ssh_requires_flag: false,
            ad_self_hosted: false,
            ad_scoring_weight: 1.0,
        }
    }

    fn parse(serialized: &str) -> ChallengeYaml {
        serde_norway::from_str(serialized).expect("serialized challenge must parse")
    }

    #[test]
    fn recognizes_only_exact_managed_image_tag_shape_for_the_game() {
        assert!(is_auto_built_tag("rsctf/42/example-service:latest", 42));
        assert!(is_auto_built_tag(
            "rsctf/42/example-service-checker:latest",
            42
        ));
        for authored in [
            "rsctf/41/example-service:latest",
            "rsctf/42/example-service:v2",
            "rsctf/42/nested/example-service:latest",
            "ghcr.io/acme/rsctf/example-service:latest",
            "registry.example/acme/rsctf/example-service@sha256:0123",
        ] {
            assert!(
                !is_auto_built_tag(authored, 42),
                "authored image was mistaken for a managed tag: {authored}"
            );
        }
    }

    #[test]
    fn source_round_trip_preserves_provide_egress_network_and_authored_image() {
        let mut challenge = challenge(ChallengeType::AttackDefense);
        challenge.container_image =
            Some("ghcr.io/acme/rsctf/example-service@sha256:0123".to_string());
        challenge.ad_allow_egress = true;
        challenge.network_mode = Some(NetworkMode::Isolated);

        let yaml = serialize_challenge_preserving_source(
            &challenge,
            &[],
            "name: old\nprovide: dist/handout.zip\n",
        )
        .unwrap();
        let parsed = parse(&yaml);
        assert_eq!(parsed.provide.as_deref(), Some("dist/handout.zip"));
        let container = parsed.container.expect("container block");
        assert_eq!(
            container.container_image.as_deref(),
            challenge.container_image.as_deref()
        );
        assert_eq!(container.network_mode, Some(NetworkMode::Isolated));
        assert_eq!(
            parsed.ad.expect("ad block").allow_egress,
            Some(true),
            "allowEgress=true must survive a push and subsequent parse"
        );
    }

    #[test]
    fn default_egress_and_network_mode_remain_sparse_and_managed_image_is_omitted() {
        let mut challenge = challenge(ChallengeType::AttackDefense);
        challenge.container_image = Some("rsctf/42/example-service:latest".to_string());

        let parsed = parse(&serialize_challenge(&challenge, &[]).unwrap());
        let container = parsed.container.expect("container block");
        assert_eq!(container.container_image, None);
        assert_eq!(container.network_mode, None);
        assert!(parsed.ad.is_none());
    }

    #[test]
    fn malformed_source_fails_closed_before_provide_can_be_lost() {
        let challenge = challenge(ChallengeType::StaticAttachment);
        assert!(
            serialize_challenge_preserving_source(&challenge, &[], "provide: [")
                .unwrap_err()
                .to_string()
                .contains("current challenge manifest is invalid")
        );
    }

    #[test]
    fn recognizes_only_managed_checker_revision_suffixes() {
        let revision = "0123456789abcdef0123456789abcdef";
        assert!(is_managed_checker_revision(
            &format!("/shared/checkers/42/service/revisions/{revision}"),
            42
        ));
        assert!(!is_managed_checker_revision(
            &format!("/shared/checkers/41/service/revisions/{revision}"),
            42
        ));
        assert!(!is_managed_checker_revision(
            "/shared/checkers/42/service/current",
            42
        ));
    }
}
