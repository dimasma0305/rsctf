#!/usr/bin/env python3
"""Implementation of the public rsctf challenge repository validation action."""

from __future__ import annotations

import json
import os
from pathlib import Path, PurePosixPath
import re
import shlex
import stat
import subprocess
import sys


RSCTF_ENTRYPOINT = "/usr/local/bin/rsctf"
ACTION_REPOSITORY_PATTERN = re.compile(
    r"^[A-Za-z0-9](?:[A-Za-z0-9_.-]{0,98}[A-Za-z0-9])?/"
    r"[A-Za-z0-9](?:[A-Za-z0-9_.-]{0,98}[A-Za-z0-9])?$"
)
DIGEST_PATTERN = re.compile(r"^sha256:[0-9a-f]{64}$")
VERSION_REF_PATTERN = re.compile(
    r"^v?(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){0,2}"
    r"(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$"
)
REVISION_PATTERN = re.compile(r"^[0-9a-f]{40}$")
VERSION_LABEL_PATTERN = re.compile(
    r"^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?(?:\+[0-9A-Za-z][0-9A-Za-z.-]*)?$"
)
MATRIX_TAG_PATTERN = re.compile(r"^[a-z0-9_][a-z0-9_.-]{0,127}$")
MAX_MATRIX_BYTES = 1024 * 1024
MAX_MATRIX_ENTRIES = 1024


class ActionError(RuntimeError):
    """The action cannot safely validate the requested repository."""


def parse_boolean(value: str, name: str) -> bool:
    normalized = value.strip().lower()
    if normalized == "true":
        return True
    if normalized == "false":
        return False
    raise ActionError(f"{name} must be true or false")


def parse_docker_command(value: str) -> list[str]:
    command = shlex.split(value)
    if not command:
        raise ActionError("DOCKER must name a Docker-compatible command")
    return command


def validate_repository_root(value: str, workspace_value: str | None = None) -> Path:
    workspace = Path(workspace_value or Path.cwd()).resolve()
    path = Path(value)
    if not path.is_absolute():
        path = workspace / path
    try:
        metadata = path.lstat()
    except OSError as error:
        raise ActionError(f"cannot inspect repository root {path}: {error}") from error
    if not stat.S_ISDIR(metadata.st_mode):
        raise ActionError(f"repository root must be a real directory: {path}")

    root = path.resolve()
    try:
        root.relative_to(workspace)
    except ValueError as error:
        raise ActionError(
            f"repository root must stay within the GitHub workspace: {root}"
        ) from error
    event = root / ".gzevent"
    try:
        event_metadata = event.lstat()
    except OSError as error:
        raise ActionError(
            f"repository root must contain a regular .gzevent file: {error}"
        ) from error
    if not stat.S_ISREG(event_metadata.st_mode):
        raise ActionError(
            "repository root must contain a regular .gzevent file, not a symlink"
        )
    if "," in os.fspath(root):
        raise ActionError(
            "repository path cannot contain a comma because Docker --mount uses "
            "comma-delimited fields"
        )
    return root


def action_image_repository(action_repository: str) -> str:
    if not ACTION_REPOSITORY_PATTERN.fullmatch(action_repository):
        raise ActionError(
            "the action repository must use GitHub's owner/repository form"
        )
    return f"ghcr.io/{action_repository.lower()}"


def image_tag_for_action_ref(action_ref: str) -> str:
    if action_ref == "main":
        return "main"
    if VERSION_REF_PATTERN.fullmatch(action_ref):
        return action_ref.removeprefix("v")
    raise ActionError(
        "this action ref cannot select a matching rsctf image automatically; "
        "use main/a version ref, or pass image with an exact @sha256 digest"
    )


def select_source_image(
    action_repository: str, action_ref: str, image_override: str
) -> tuple[str, str]:
    repository = action_image_repository(action_repository)
    if not image_override:
        return f"{repository}:{image_tag_for_action_ref(action_ref)}", repository

    prefix = f"{repository}@"
    if not image_override.startswith(prefix):
        raise ActionError(
            f"image must come from the action's platform package {repository}"
        )
    digest = image_override[len(prefix) :]
    if not DIGEST_PATTERN.fullmatch(digest):
        raise ActionError(
            "image must end in @sha256:<64 lowercase hex characters>; mutable "
            "image overrides are not accepted"
        )
    return image_override, repository


def run_process(
    command: list[str], root: Path, timeout: int, capture_output: bool = False
) -> subprocess.CompletedProcess[str]:
    try:
        return subprocess.run(
            command,
            cwd=root,
            check=False,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE if capture_output else None,
            stderr=subprocess.PIPE if capture_output else None,
            text=True,
            timeout=timeout,
        )
    except FileNotFoundError as error:
        raise ActionError(f"required command was not found: {command[0]}") from error
    except subprocess.TimeoutExpired as error:
        raise ActionError(
            f"command exceeded its {timeout}-second timeout: {command[0]}"
        ) from error


def require_success(
    result: subprocess.CompletedProcess[str], operation: str
) -> subprocess.CompletedProcess[str]:
    if result.returncode == 0:
        return result
    detail = (result.stderr or result.stdout or "").strip()
    suffix = f": {detail}" if detail else ""
    raise ActionError(f"{operation} failed with status {result.returncode}{suffix}")


def pull_and_resolve_image(
    docker: list[str], source_image: str, repository: str, root: Path
) -> str:
    require_success(
        run_process([*docker, "pull", source_image], root, timeout=600),
        f"pulling {source_image}",
    )
    result = require_success(
        run_process(
            [
                *docker,
                "image",
                "inspect",
                "--format",
                "{{json .RepoDigests}}",
                source_image,
            ],
            root,
            timeout=30,
            capture_output=True,
        ),
        f"inspecting {source_image}",
    )
    try:
        repo_digests = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ActionError("Docker returned malformed RepoDigests JSON") from error
    if not isinstance(repo_digests, list):
        raise ActionError("Docker did not return a RepoDigests array")

    prefix = f"{repository}@"
    matching = sorted(
        {
            value
            for value in repo_digests
            if isinstance(value, str)
            and value.startswith(prefix)
            and DIGEST_PATTERN.fullmatch(value[len(prefix) :])
        }
    )
    if source_image.startswith(prefix):
        if source_image not in matching:
            raise ActionError(
                "Docker did not retain the requested immutable image digest"
            )
        return source_image
    if len(matching) != 1:
        raise ActionError(
            f"expected one immutable RepoDigest for {repository}, found {len(matching)}"
        )
    return matching[0]


def inspect_labels(
    docker: list[str], image: str, root: Path
) -> dict[str, str]:
    result = require_success(
        run_process(
            [
                *docker,
                "image",
                "inspect",
                "--format",
                "{{json .Config.Labels}}",
                image,
            ],
            root,
            timeout=30,
            capture_output=True,
        ),
        f"inspecting labels for {image}",
    )
    try:
        labels = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ActionError("Docker returned malformed image-label JSON") from error
    if not isinstance(labels, dict) or any(
        not isinstance(key, str) or not isinstance(value, str)
        for key, value in labels.items()
    ):
        raise ActionError("rsctf image labels must be a string mapping")
    return labels


def validate_labels(
    labels: dict[str, str], action_repository: str, action_ref: str
) -> str:
    expected_source = f"https://github.com/{action_repository}".lower()
    if labels.get("org.opencontainers.image.source", "").lower() != expected_source:
        raise ActionError("image source label does not match the rsctf action repository")
    if labels.get("org.opencontainers.image.title") != "rsctf":
        raise ActionError("image title label is not rsctf")
    revision = labels.get("org.opencontainers.image.revision", "")
    if not REVISION_PATTERN.fullmatch(revision):
        raise ActionError("image revision label is not a full Git commit")
    if REVISION_PATTERN.fullmatch(action_ref) and revision != action_ref.lower():
        raise ActionError(
            "commit-pinned action ref does not match the rsctf image revision"
        )
    version = labels.get("org.opencontainers.image.version", "")
    if not VERSION_LABEL_PATTERN.fullmatch(version):
        raise ActionError("image version label is not a valid full semantic version")

    if VERSION_REF_PATTERN.fullmatch(action_ref):
        requested = action_ref.removeprefix("v")
        numeric_components = requested.split("-", 1)[0].count(".") + 1
        if numeric_components == 3:
            matches = version == requested
        else:
            matches = version == requested or version.startswith(f"{requested}.")
        if not matches:
            raise ActionError(
                f"action ref {action_ref} selected rsctf image version {version}"
            )
    return version


def docker_run_command(
    docker: list[str], image: str, root: Path, arguments: list[str]
) -> list[str]:
    return [
        *docker,
        "run",
        "--rm",
        "--pull=never",
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges=true",
        "--user=65534:65534",
        "--mount",
        f"type=bind,source={root},target=/repository,readonly",
        "--entrypoint",
        RSCTF_ENTRYPOINT,
        image,
        *arguments,
    ]


def github_escape(value: str) -> str:
    return value.replace("%", "%25").replace("\r", "%0D").replace("\n", "%0A")


def has_control_characters(value: str) -> bool:
    return any(ord(character) < 32 or ord(character) == 127 for character in value)


def normalize_container_matrix(value: str) -> tuple[str, int]:
    if len(value.encode("utf-8")) > MAX_MATRIX_BYTES:
        raise ActionError("rsctf container matrix exceeds the action size limit")
    try:
        payload = json.loads(value)
    except json.JSONDecodeError as error:
        raise ActionError("rsctf returned malformed container-matrix JSON") from error
    if not isinstance(payload, dict) or set(payload) != {"include"}:
        raise ActionError("rsctf container matrix must contain only include")
    entries = payload["include"]
    if not isinstance(entries, list) or len(entries) > MAX_MATRIX_ENTRIES:
        raise ActionError("rsctf container matrix include must be a bounded array")
    names: set[str] = set()
    contexts: set[str] = set()
    tags: set[str] = set()
    for entry in entries:
        if not isinstance(entry, dict) or set(entry) != {
            "name",
            "context",
            "tag",
            "kind",
        }:
            raise ActionError("rsctf container matrix entry has an invalid shape")
        name = entry["name"]
        context = entry["context"]
        tag = entry["tag"]
        kind = entry["kind"]
        if (
            not isinstance(name, str)
            or not name
            or len(name) > 1024
            or has_control_characters(name)
        ):
            raise ActionError("rsctf container matrix entry has an invalid name")
        if not isinstance(context, str) or not context or "\\" in context:
            raise ActionError("rsctf container matrix entry has an invalid context")
        path = PurePosixPath(context)
        if (
            path.is_absolute()
            or path.as_posix() != context
            or any(part in {"", ".", ".."} for part in context.split("/"))
            or has_control_characters(context)
        ):
            raise ActionError("rsctf container matrix context must be a safe relative path")
        if not isinstance(tag, str) or not MATRIX_TAG_PATTERN.fullmatch(tag):
            raise ActionError("rsctf container matrix entry has an invalid Docker tag")
        if kind not in {"service", "generator"}:
            raise ActionError("rsctf container matrix entry has an invalid kind")
        if name in names or context in contexts or tag in tags:
            raise ActionError("rsctf container matrix entries must be unique")
        names.add(name)
        contexts.add(context)
        tags.add(tag)
    return json.dumps(payload, separators=(",", ":"), sort_keys=True), len(entries)


def write_output(name: str, value: str) -> None:
    output = os.environ.get("GITHUB_OUTPUT")
    if not output:
        return
    with open(output, "a", encoding="utf-8") as stream:
        stream.write(f"{name}={value}\n")


def main() -> int:
    annotations = True
    try:
        annotations = parse_boolean(
            os.environ.get("RSCTF_ACTION_GITHUB_ANNOTATIONS", "true"),
            "github-annotations",
        )
        deny_warnings = parse_boolean(
            os.environ.get("RSCTF_ACTION_DENY_WARNINGS", "true"),
            "deny-warnings",
        )
        root = validate_repository_root(
            os.environ.get("RSCTF_ACTION_PATH_INPUT", "."),
            os.environ.get("GITHUB_WORKSPACE"),
        )
        action_repository = os.environ.get("RSCTF_ACTION_REPOSITORY", "")
        action_ref = os.environ.get("RSCTF_ACTION_REF", "")
        source_image, image_repository = select_source_image(
            action_repository,
            action_ref,
            os.environ.get("RSCTF_ACTION_IMAGE", ""),
        )
        docker = parse_docker_command(os.environ.get("DOCKER", "docker"))

        print(f"Pulling rsctf validator from {source_image}", flush=True)
        image = pull_and_resolve_image(docker, source_image, image_repository, root)
        labels = inspect_labels(docker, image, root)
        image_version = validate_labels(labels, action_repository, action_ref)

        version_result = require_success(
            run_process(
                docker_run_command(
                    docker, image, root, ["challenge", "check", "--version"]
                ),
                root,
                timeout=120,
                capture_output=True,
            ),
            "running rsctf challenge check --version",
        )
        version_output = version_result.stdout.strip()
        version_prefix = "rsctf "
        if not version_output.startswith(version_prefix):
            raise ActionError("rsctf challenge check returned an invalid version string")
        cli_version = version_output.removeprefix(version_prefix)
        if cli_version != image_version:
            raise ActionError(
                f"rsctf version {cli_version} does not match image version {image_version}"
            )
        print(version_output, flush=True)

        arguments = ["challenge", "check"]
        if annotations:
            arguments.append("--github")
        if deny_warnings:
            arguments.append("--deny-warnings")
        arguments.append("/repository")
        result = run_process(
            docker_run_command(docker, image, root, arguments),
            root,
            timeout=300,
        )
        if result.returncode != 0:
            return result.returncode

        matrix_result = require_success(
            run_process(
                docker_run_command(
                    docker, image, root, ["challenge", "matrix", "/repository"]
                ),
                root,
                timeout=300,
                capture_output=True,
            ),
            "running rsctf challenge matrix",
        )
        container_matrix, container_count = normalize_container_matrix(
            matrix_result.stdout.strip()
        )

        write_output("image", image)
        write_output("version", cli_version)
        write_output("container_matrix", container_matrix)
        write_output("container_count", str(container_count))
        return 0
    except ActionError as error:
        message = github_escape(str(error))
        if annotations or os.environ.get("GITHUB_ACTIONS") == "true":
            print(f"::error title=rsctf challenge validation::{message}", file=sys.stderr)
        else:
            print(f"error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
