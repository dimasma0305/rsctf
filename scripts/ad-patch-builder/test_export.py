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

from build import private_archive
from export import export_candidate


class ExportTests(unittest.TestCase):
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
            CREATE TABLE "AdPatchCandidates"(service_id int PRIMARY KEY, revision int, digest text, archive_bytes int);
            CREATE TABLE "AdPatchCandidateChunks"(service_id int, part int, payload bytea, PRIMARY KEY(service_id,part));
            CREATE TABLE "AdPatchBuilds"(service_id int, revision int, attempt uuid, status text);
            CREATE TABLE "AdPatchBuildEligibleServices"(service_id int PRIMARY KEY);
        ''')
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

    def export(self):
        return export_candidate(self.db, 1, 4, self.attempt, self.path)

    def test_verified_private_file_does_not_replace_an_existing_file(self):
        self.assertEqual(self.export(), self.digest)
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


if __name__ == "__main__":
    unittest.main()
