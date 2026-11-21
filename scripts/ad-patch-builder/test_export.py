"""Exercise private candidate transfer against a disposable PostgreSQL database."""

from contextlib import contextmanager
from concurrent.futures import ThreadPoolExecutor
import hashlib
import os
from pathlib import Path
import stat
import tempfile
from threading import Barrier
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import uuid

import psycopg
from psycopg import sql

from build import ImageReceipt, private_archive
from export import export_candidate
from stage import service_repository, stage_candidate


class CandidateFixture(unittest.TestCase):
    def setUp(self):
        url = os.environ["RSCTF_TEST_DATABASE_URL"]
        self.db = psycopg.connect(url, autocommit=True)
        self.addCleanup(self.db.close)
        self.other = psycopg.connect(url, autocommit=True)
        self.addCleanup(self.other.close)
        schema = sql.Identifier("export_test_" + uuid.uuid4().hex)
        self.db.execute(sql.SQL("CREATE SCHEMA {}").format(schema))
        self.addCleanup(self.db.execute, sql.SQL("DROP SCHEMA {} CASCADE").format(schema))
        for connection in (self.db, self.other):
            connection.execute(sql.SQL("SET search_path TO {}").format(schema))
        self.db.execute('''
            CREATE TABLE "AdPatchCandidates"(service_id int PRIMARY KEY, revision int, digest text, archive_bytes int,
                                             manifest_digest text, config_digest text);
            CREATE TABLE "AdPatchCandidateChunks"(service_id int, part int, payload bytea, PRIMARY KEY(service_id,part));
            CREATE TABLE "AdPatchBuilds"(service_id int, revision int, attempt uuid, status text);
            CREATE TABLE "AdPatchBuildEligibleServices"(service_id int PRIMARY KEY);
        ''')
        migration = Path(__file__).resolve().parents[2] / "src/migrations/m0375_ad_patch_registry_receipt.rs"
        self.db.execute(migration.read_text().split('r#"', 1)[1].split('"#;', 1)[0])
        self.attempt = uuid.uuid4()
        self.first = b"a" * 1048576
        self.data = self.first + b"tail"
        self.digest = hashlib.sha256(self.data).hexdigest()
        self.db.execute('INSERT INTO "AdPatchBuildEligibleServices" VALUES (1), (2)')
        self.db.execute('INSERT INTO "AdPatchBuilds" VALUES (1,4,%s,\'Ready\')', (self.attempt,))
        self.db.execute('INSERT INTO "AdPatchCandidates" VALUES (1,4,%s,%s)', (self.digest, len(self.data)))
        self.db.execute('INSERT INTO "AdPatchCandidateChunks" VALUES (1,1,%s),(1,0,%s)', (b"tail", self.first))
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.path = Path(tmp.name) / "candidate.oci.tar"


class ExportTests(CandidateFixture):
    def export(self):
        return export_candidate(self.db, 1, 4, self.attempt, self.path)

    def test_verified_private_file_does_not_replace_an_existing_file(self):
        self.assertEqual(self.export(), ImageReceipt(self.digest, None, None))
        self.assertEqual(self.path.read_bytes(), self.data)
        self.assertEqual(stat.S_IMODE(self.path.stat().st_mode), 0o600)
        self.path.write_bytes(b"existing")
        with self.assertRaises(FileExistsError):
            self.export()
        self.assertEqual(self.path.read_bytes(), b"existing")

    def test_exact_eligible_service_revision_and_attempt_required(self):
        for identity in ((2, 4, self.attempt), (1, 5, self.attempt), (1, 4, uuid.uuid4())):
            with self.subTest(identity=identity), self.assertRaisesRegex(ValueError, "ready candidate"):
                export_candidate(self.db, *identity, self.path)
        self.db.execute('DELETE FROM "AdPatchBuildEligibleServices" WHERE service_id=1')
        with self.assertRaisesRegex(ValueError, "ready candidate"):
            self.export()
        self.assertFalse(self.path.exists())

    def test_export_uses_image_identity_from_selected_candidate(self):
        manifest, config = ("sha256:" + c * 64 for c in "ab")
        self.db.execute('UPDATE "AdPatchCandidates" SET manifest_digest=%s, config_digest=%s',
                        (manifest, config))
        self.assertEqual(self.export(), ImageReceipt(self.digest, manifest, config))

    def test_concurrent_exports_publish_only_one_file(self):
        barrier = Barrier(2, timeout=10)

        @contextmanager
        def finish_together(path):
            with private_archive(path) as file:
                yield file
                barrier.wait()

        with patch("export.private_archive", finish_together), ThreadPoolExecutor(2) as executor:
            writes = [executor.submit(export_candidate, db, 1, 4, self.attempt, self.path)
                      for db in (self.db, self.other)]
            outcomes = [write.exception() for write in writes]
        self.assertEqual(outcomes.count(None), 1)
        self.assertEqual(sum(isinstance(error, FileExistsError) for error in outcomes), 1)
        self.assertEqual(list(self.path.parent.iterdir()), [self.path])
        self.assertEqual(self.path.read_bytes(), self.data)

    def test_damaged_or_incomplete_archive_is_not_published(self):
        for change in (
            'UPDATE "AdPatchCandidateChunks" SET payload=\'fail\' WHERE part=1',
            'UPDATE "AdPatchCandidateChunks" SET part=2 WHERE part=1',
            'DELETE FROM "AdPatchCandidateChunks" WHERE part=1',
        ):
            with self.subTest(change=change):
                self.db.execute(change)
                with self.assertRaises(ValueError):
                    self.export()
                self.assertEqual(list(self.path.parent.iterdir()), [])
                self.db.execute('DELETE FROM "AdPatchCandidateChunks" WHERE part<>0')
                self.db.execute('INSERT INTO "AdPatchCandidateChunks" VALUES (1,1,%s)', (b"tail",))

    def test_retirement_during_copy_discards_the_export(self):
        @contextmanager
        def retire_after_first_chunk(path):
            with private_archive(path) as file:
                def write(data):
                    if file.tell() == 0:
                        with self.other.transaction():
                            self.other.execute('DELETE FROM "AdPatchCandidateChunks"')
                            self.other.execute('DELETE FROM "AdPatchCandidates"')
                            self.other.execute('UPDATE "AdPatchBuilds" SET status=\'Replaced\'')
                    return file.write(data)
                yield SimpleNamespace(write=write)
        with patch("export.private_archive", retire_after_first_chunk):
            with self.assertRaisesRegex(ValueError, "changed during export"):
                self.export()
        self.assertEqual(list(self.path.parent.iterdir()), [])


class StagingTests(CandidateFixture):
    def setUp(self):
        super().setUp()
        self.prefix = "registry.example/private"
        self.manifest, self.config = ("sha256:" + c * 64 for c in "ab")
        self.db.execute('UPDATE "AdPatchCandidates" SET manifest_digest=%s, config_digest=%s',
                        (self.manifest, self.config))

    def stage(self, db=None, prefix=None):
        return stage_candidate(db or self.db, 1, 4, self.attempt, prefix or self.prefix)

    def stored_ref(self):
        return self.db.execute('SELECT registry_image_ref FROM "AdPatchCandidates"').fetchone()[0]

    def test_repository_scope(self):
        for prefix in ("unqualified/project", "registry.example", "registry.example/p:tag",
                       "https://registry.example/p", "user@registry.example/p", "registry.example/a/../b",
                       "registry.example:65536/project", "registry.example/p@sha256:abc"):
            with self.subTest(prefix=prefix), self.assertRaises(ValueError):
                service_repository(prefix, 1)
        with self.assertRaises(psycopg.errors.CheckViolation):
            self.db.execute('UPDATE "AdPatchCandidates" SET registry_image_ref=%s',
                            (f"{self.prefix}/service-2@{self.manifest}",))

    def test_idempotent_scoped_receipt_and_single_registry_writer(self):
        def concurrent(*args):
            with self.assertRaisesRegex(ValueError, "another registry transfer"):
                self.stage(self.other, "registry.example/other")
        with patch("stage.push_candidate", side_effect=concurrent) as push:
            ref = self.stage()
            self.assertEqual(ref, f"{self.prefix}/service-1@{self.manifest}")
            self.assertEqual(self.stored_ref(), ref)
            self.assertEqual(self.stage(), ref)
            with self.assertRaisesRegex(ValueError, "different repository"):
                self.stage(prefix="registry.example/other")
            self.assertEqual(push.call_count, 2)

    def test_revocation_during_upload_and_transport_failure(self):
        with patch("stage.push_candidate", side_effect=OSError("offline")), self.assertRaises(OSError):
            self.stage()
        self.assertIsNone(self.stored_ref())
        def revoke(*args):
            self.other.execute('DELETE FROM "AdPatchBuildEligibleServices" WHERE service_id=1')
        with patch("stage.push_candidate", side_effect=revoke), self.assertRaisesRegex(ValueError, "eligibility"):
            self.stage()
        self.assertIsNone(self.stored_ref())

    def test_reject_other_service_stale_attempt_and_legacy_guest_before_upload(self):
        with patch("stage.push_candidate") as push:
            for identity in ((2, 4, self.attempt), (1, 5, self.attempt), (1, 4, uuid.uuid4())):
                with self.subTest(identity=identity), self.assertRaisesRegex(ValueError, "ready candidate"):
                    stage_candidate(self.db, *identity, self.prefix)
            self.db.execute('UPDATE "AdPatchCandidates" SET config_digest=NULL,manifest_digest=NULL')
            with self.assertRaisesRegex(ValueError, "rebuild"):
                self.stage()
            push.assert_not_called()
        self.assertIsNone(self.stored_ref())


if __name__ == "__main__":
    unittest.main()
