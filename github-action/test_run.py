#!/usr/bin/env python3
"""Regression tests for the public rsctf challenge validation action."""

from __future__ import annotations

from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from run import (
    ActionError,
    RSCTF_ENTRYPOINT,
    docker_run_command,
    image_tag_for_action_ref,
    parse_boolean,
    select_source_image,
    main,
    validate_labels,
    validate_repository_root,
)


ACTION_REPOSITORY = "dimasma0305/rsctf"
IMAGE_REPOSITORY = "ghcr.io/dimasma0305/rsctf"
VALID_IMAGE = IMAGE_REPOSITORY + "@sha256:" + "a" * 64


class ChallengeCheckActionTests(unittest.TestCase):
    def test_boolean_inputs_are_strict(self) -> None:
        self.assertTrue(parse_boolean("true", "value"))
        self.assertFalse(parse_boolean("FALSE", "value"))
        with self.assertRaises(ActionError):
            parse_boolean("yes", "value")

    def test_action_refs_select_matching_image_tags(self) -> None:
        self.assertEqual(image_tag_for_action_ref("main"), "main")
        self.assertEqual(image_tag_for_action_ref("v0.2.3"), "0.2.3")
        self.assertEqual(image_tag_for_action_ref("v0"), "0")
        with self.assertRaises(ActionError):
            image_tag_for_action_ref("feature/untrusted")

    def test_image_override_must_be_immutable_and_from_action_repository(self) -> None:
        self.assertEqual(
            select_source_image(ACTION_REPOSITORY, "main", VALID_IMAGE),
            (VALID_IMAGE, IMAGE_REPOSITORY),
        )
        for image in [
            IMAGE_REPOSITORY + ":latest",
            IMAGE_REPOSITORY + "@sha256:short",
            "ghcr.io/elsewhere/rsctf@sha256:" + "a" * 64,
        ]:
            with self.subTest(image=image):
                with self.assertRaises(ActionError):
                    select_source_image(ACTION_REPOSITORY, "main", image)

    def test_repository_must_have_a_regular_event_manifest(self) -> None:
        with tempfile.TemporaryDirectory(prefix="rsctf-action-") as directory:
            root = Path(directory)
            with self.assertRaises(ActionError):
                validate_repository_root(".", directory)
            (root / ".gzevent").write_text("hidden: true\n", encoding="utf-8")
            self.assertEqual(validate_repository_root(".", directory), root.resolve())
            with self.assertRaises(ActionError):
                validate_repository_root("/", directory)

    def test_image_labels_are_bound_to_action_repository_and_version(self) -> None:
        labels = {
            "org.opencontainers.image.source": "https://github.com/dimasma0305/rsctf",
            "org.opencontainers.image.title": "rsctf",
            "org.opencontainers.image.revision": "b" * 40,
            "org.opencontainers.image.version": "0.2.3",
        }
        self.assertEqual(
            validate_labels(labels, ACTION_REPOSITORY, "v0.2.3"), "0.2.3"
        )
        self.assertEqual(validate_labels(labels, ACTION_REPOSITORY, "v0"), "0.2.3")
        with self.assertRaises(ActionError):
            validate_labels(labels, ACTION_REPOSITORY, "v0.2.4")
        with self.assertRaises(ActionError):
            validate_labels(
                {**labels, "org.opencontainers.image.title": "other"},
                ACTION_REPOSITORY,
                "main",
            )
        with self.assertRaises(ActionError):
            validate_labels(labels, ACTION_REPOSITORY, "c" * 40)
        with self.assertRaises(ActionError):
            validate_labels(
                {**labels, "org.opencontainers.image.version": "0.2.3\nforged=value"},
                ACTION_REPOSITORY,
                "main",
            )

    def test_command_runs_fixed_binary_in_read_only_offline_sandbox(self) -> None:
        root = Path("/workspace/challenges")
        command = docker_run_command(
            ["docker"],
            VALID_IMAGE,
            root,
            ["challenge", "check", "--github", "/repository"],
        )
        self.assertEqual(command[0:2], ["docker", "run"])
        for option in [
            "--pull=never",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges=true",
            "--user=65534:65534",
        ]:
            self.assertIn(option, command)
        self.assertIn(
            "type=bind,source=/workspace/challenges,target=/repository,readonly",
            command,
        )
        self.assertEqual(command[command.index("--entrypoint") + 1], RSCTF_ENTRYPOINT)
        self.assertEqual(
            command[-4:], ["challenge", "check", "--github", "/repository"]
        )

    def test_action_resolves_validates_and_runs_the_platform_image(self) -> None:
        with tempfile.TemporaryDirectory(prefix="rsctf-action-") as directory:
            root = Path(directory)
            (root / ".gzevent").write_text("hidden: true\n", encoding="utf-8")
            output = root / "github-output"
            labels = {
                "org.opencontainers.image.source": (
                    "https://github.com/dimasma0305/rsctf"
                ),
                "org.opencontainers.image.title": "rsctf",
                "org.opencontainers.image.revision": "b" * 40,
                "org.opencontainers.image.version": "0.2.3",
            }
            calls: list[list[str]] = []

            def fake_run_process(
                command: list[str],
                _root: Path,
                timeout: int,
                capture_output: bool = False,
            ) -> subprocess.CompletedProcess[str]:
                del capture_output, timeout
                calls.append(command)
                if command[1:3] == ["image", "inspect"]:
                    template = command[command.index("--format") + 1]
                    value = (
                        json.dumps([VALID_IMAGE])
                        if template == "{{json .RepoDigests}}"
                        else json.dumps(labels)
                    )
                    return subprocess.CompletedProcess(command, 0, value, "")
                if command[-3:] == ["challenge", "check", "--version"]:
                    return subprocess.CompletedProcess(
                        command, 0, "rsctf 0.2.3\n", ""
                    )
                return subprocess.CompletedProcess(command, 0, "", "")

            environment = {
                "DOCKER": "docker",
                "GITHUB_ACTIONS": "true",
                "GITHUB_OUTPUT": os.fspath(output),
                "GITHUB_WORKSPACE": directory,
                "RSCTF_ACTION_DENY_WARNINGS": "true",
                "RSCTF_ACTION_GITHUB_ANNOTATIONS": "true",
                "RSCTF_ACTION_IMAGE": VALID_IMAGE,
                "RSCTF_ACTION_PATH_INPUT": ".",
                "RSCTF_ACTION_REF": "v0.2.3",
                "RSCTF_ACTION_REPOSITORY": ACTION_REPOSITORY,
            }
            with patch.dict(os.environ, environment, clear=True), patch(
                "run.run_process", side_effect=fake_run_process
            ), redirect_stdout(io.StringIO()):
                self.assertEqual(main(), 0)

            self.assertIn(["docker", "pull", VALID_IMAGE], calls)
            validation = calls[-1]
            self.assertEqual(
                validation[-5:],
                [
                    "challenge",
                    "check",
                    "--github",
                    "--deny-warnings",
                    "/repository",
                ],
            )
            self.assertEqual(
                output.read_text(encoding="utf-8"),
                f"image={VALID_IMAGE}\nversion=0.2.3\n",
            )


if __name__ == "__main__":
    unittest.main()
