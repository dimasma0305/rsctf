"""Regression tests for incomplete release bumps and stale package locks."""
from pathlib import Path
import runpy
import tempfile
import unittest

module = runpy.run_path(str(Path(__file__).with_name("check-release-versions.py")))
check_versions = module["check_versions"]


class ReleaseVersionsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name, directory in module["PACKAGES"].items():
            (self.root / directory).mkdir(parents=True, exist_ok=True)
            for filename, header in (("Cargo.toml", "[package]"), ("Cargo.lock", "[[package]]")):
                (self.root / directory / filename).write_text(f'{header}\nname = "{name}"\nversion = "1.2.3"\n')
        chart = self.root / "charts/rsctf/Chart.yaml"
        chart.parent.mkdir(parents=True)
        chart.write_text('version: 1.2.3\nappVersion: "1.2.3"\n')

    def test_complete_bump_and_requested_version(self):
        self.assertEqual(check_versions(self.root), "1.2.3")
        self.assertEqual(check_versions(self.root, "1.2.3"), "1.2.3")
        for requested in ("1.2.4", "v1.2.3", ""):
            with self.subTest(requested=requested), self.assertRaises(ValueError):
                check_versions(self.root, requested)

    def test_each_manifest_and_lock_must_match(self):
        for path in list(self.root.rglob("Cargo.toml")) + list(self.root.rglob("Cargo.lock")):
            original = path.read_text()
            path.write_text(original.replace("1.2.3", "1.2.2"))
            with self.subTest(path=path), self.assertRaises(ValueError):
                check_versions(self.root)
            path.write_text(original)

    def test_chart_fields_cannot_be_stale_missing_or_duplicated(self):
        chart = self.root / "charts/rsctf/Chart.yaml"
        original = chart.read_text()
        for content in (original.replace("1.2.3", "1.2.2", 1), 'version: 1.2.3\n',
                        original + "version: 1.2.3\n", 'version: 1.2.3\nappVersion: 1.2.2\n'):
            chart.write_text(content)
            with self.subTest(content=content), self.assertRaises(ValueError):
                check_versions(self.root)

    def test_lock_must_identify_one_matching_package(self):
        path = self.root / "agents/byoc-agent/Cargo.lock"
        original = path.read_text()
        for content in (original.replace("rsctf-byoc-agent", "unrelated"), original + original):
            path.write_text(content)
            with self.subTest(content=content), self.assertRaises(ValueError):
                check_versions(self.root)


if __name__ == "__main__":
    unittest.main()
