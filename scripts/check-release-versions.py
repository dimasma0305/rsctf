"""Check coordinated package, lockfile and chart versions before publication."""
import argparse
from pathlib import Path
import re
import sys
import tomllib

PACKAGES = {
    "rsctf": Path("."),
    "rsctf-byoc-agent": Path("agents/byoc-agent"),
    "rsctf-worker-agent": Path("agents/worker-agent"),
}
SEMVER = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:[+-][0-9A-Za-z.-]+)?")


def check_versions(root: Path, expected: str | None = None) -> str:
    versions = {}
    for name, directory in PACKAGES.items():
        manifest = root / directory / "Cargo.toml"
        with manifest.open("rb") as source:
            package = tomllib.load(source)["package"]
        if package.get("name") != name:
            raise ValueError(f"Unexpected package name in {manifest}")
        versions[str(directory / "Cargo.toml")] = package["version"]
        with (root / directory / "Cargo.lock").open("rb") as source:
            matches = [p for p in tomllib.load(source)["package"] if p["name"] == name]
        if len(matches) != 1:
            raise ValueError(f"Expected one {name} package in {directory / 'Cargo.lock'}")
        versions[str(directory / "Cargo.lock")] = matches[0]["version"]

    # Only these two top-level scalar fields are release metadata. Reject missing,
    # duplicate or unsupported values rather than accepting a partial YAML parse.
    chart = (root / "charts/rsctf/Chart.yaml").read_text()
    for key in ("version", "appVersion"):
        matches = re.findall(rf"^{key}:[ \t]*(?:\"([^\"]+)\"|'([^']+)'|([^ \t#\n]+))[ \t]*(?:#.*)?$", chart, re.M)
        if len(matches) != 1:
            raise ValueError(f"Expected one scalar {key} in Chart.yaml")
        versions[f"Chart.yaml {key}"] = next(value for value in matches[0] if value)

    expected = versions["Cargo.toml"] if expected is None else expected
    if not SEMVER.fullmatch(expected):
        raise ValueError(f"Invalid release version: {expected}")
    mismatches = [f"{source}={version}" for source, version in versions.items() if version != expected]
    if mismatches:
        raise ValueError(f"Release {expected} does not match " + ", ".join(mismatches))
    return expected


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--release-version")
    args = parser.parse_args()
    try:
        version = check_versions(args.root, args.release_version)
    except (OSError, ValueError, KeyError) as error:
        print(error, file=sys.stderr)
        sys.exit(1)
    print(f"Coordinated versions match: {version} (3 packages, 3 lockfiles, chart and appVersion)")
