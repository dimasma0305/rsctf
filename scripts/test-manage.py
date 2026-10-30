#!/usr/bin/env python3
"""Maintenance recovery boundaries; optional real Docker/PostgreSQL restore drill."""
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('manage', Path(__file__).with_name('manage.py'))
manage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(manage)
IMAGE = 'ghcr.io/dimasma0305/rsctf@sha256:3eb2a3591ac3acc41664d2120f8a26dbf9dc519b0fc888c1ec31a821e121db7b'


def archive(path, name='upload.txt', data=b'uploaded evidence'):
    with tarfile.open(path, 'w') as handle:
        member = tarfile.TarInfo(name)
        member.size = len(data)
        member.uid, member.gid, member.mode = 1234, 2345, 0o640
        handle.addfile(member, io.BytesIO(data))


class Fixture:
    def __init__(self, root):
        self.root = root
        self.deploy = root / 'deploy'
        self.deploy.mkdir()
        self.env_path = self.deploy / '.env'
        self.env_path.write_text('RSCTF_IMAGE=' + IMAGE + '\n')
        (self.deploy / 'release.env').write_text('RSCTF_RELEASE_VERSION=v0.1.156\n')
        self.image = IMAGE
        self.running = True
        self.events = []
        self.copy_failure = False

    def runtime(self):
        return {'State': {'Running': self.running}}

    def call(self, *args, **kwargs):
        self.events.append(args[0])
        if args[0] == 'stop': self.running = False
        if args[0] == 'start': self.running = True
        if args[0] == 'exec':
            assert not self.running, 'database snapshot must have no application writer'
            kwargs['stdout'].write(b'PGDMP-test-fixture')

    def copy_files(self, destination):
        assert not self.running, 'file snapshot must have no application writer'
        if self.copy_failure: raise RuntimeError('injected copy failure')
        archive(destination)


class Maintenance(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='rsctf-manage-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.installation = Fixture(self.root)

    def test_backup_stops_both_writers_and_resumes_after_complete_snapshot(self):
        saved = manage.backup(self.installation)
        self.assertEqual(self.installation.events, ['stop', 'exec', 'start'])
        self.assertTrue(self.installation.running)
        self.assertEqual(manage.verify_backup(saved)['image'], IMAGE)
        self.assertEqual(saved.stat().st_mode & 0o777, 0o700)
        self.assertEqual((saved / 'settings.env').stat().st_mode & 0o777, 0o600)
        (saved / 'database.dump').write_bytes(b'corrupt')
        with self.assertRaisesRegex(RuntimeError, 'verification failed'):
            manage.verify_backup(saved)

    def test_failed_backup_resumes_original_and_does_not_publish_manifest(self):
        self.installation.copy_failure = True
        with self.assertRaisesRegex(RuntimeError, 'injected'):
            manage.backup(self.installation, resume=False)
        self.assertTrue(self.installation.running)
        self.assertEqual(list((self.root / 'backups').glob('*/manifest.json')), [])

    def test_pending_migration_prevents_backup_and_old_image_restart(self):
        (self.root / '.rsctf-update.json').write_text('{}')
        self.installation.running = False
        with self.assertRaisesRegex(RuntimeError, 'interrupted update'):
            manage.backup(self.installation)
        self.assertEqual(self.installation.events, [])

    def test_archive_escape_rejected_before_restore(self):
        for name in ('../escape', '/etc/passwd', 'safe/../../escape'):
            path = self.root / 'bad.tar'
            archive(path, name)
            with self.assertRaisesRegex(RuntimeError, 'unsafe'):
                manage.validate_files_archive(path)
        path = self.root / 'link.tar'
        with tarfile.open(path, 'w') as handle:
            member = tarfile.TarInfo('link')
            member.type = tarfile.SYMTYPE
            member.linkname = '/etc'
            handle.addfile(member)
        with self.assertRaisesRegex(RuntimeError, 'unsafe link'):
            manage.validate_files_archive(path)

    def test_restore_refuses_existing_destination_before_download(self):
        saved = manage.backup(self.installation)
        with patch.object(manage, 'download') as download:
            with self.assertRaisesRegex(RuntimeError, 'new folder'):
                manage.restore(saved, self.root, 8081)
            download.assert_not_called()

    def test_update_state_is_atomically_replaceable_and_private(self):
        path = self.root / 'state.json'
        manage.private_json(path, {'phase': 'backup'})
        manage.private_json(path, {'phase': 'cutover'})
        self.assertEqual(json.loads(path.read_text()), {'phase': 'cutover'})
        self.assertEqual(path.stat().st_mode & 0o777, 0o600)

    def test_duplicate_settings_are_rejected(self):
        path = self.root / 'duplicate.env'
        path.write_text('RSCTF_IMAGE=one\nRSCTF_IMAGE=two\n')
        with self.assertRaisesRegex(RuntimeError, 'duplicate'):
            manage.read_env(path)

    def test_cutover_failure_retry_never_restarts_old_image_or_repeats_backup(self):
        candidate = self.root / '.releases' / 'candidate'
        (candidate / 'deploy').mkdir(parents=True)
        (candidate / 'scripts').mkdir()
        (self.root / 'scripts').mkdir()
        (candidate / 'rsctf').write_text('new launcher')
        image = 'ghcr.io/dimasma0305/rsctf@sha256:' + 'a' * 64
        self.installation.values = {'RSCTF_IMAGE': IMAGE}
        self.installation.project = 'rsctf-fixture'
        state = {'candidate': str(candidate), 'image': image, 'phase': 'cutover', 'sourceImage': IMAGE}
        state_path = self.root / '.rsctf-update.json'
        manage.private_json(state_path, state)
        next_install = type('Candidate', (), {'environment': {}})()
        with patch.object(manage, 'Installation', return_value=next_install), patch.object(manage, 'backup') as backup:
            with patch.object(manage, 'run', side_effect=RuntimeError('injected migration failure')):
                with self.assertRaisesRegex(RuntimeError, 'migration failure'):
                    manage.update(self.installation)
            self.assertEqual(json.loads(state_path.read_text())['phase'], 'cutover')
            self.assertEqual(manage.read_env(self.installation.env_path)['RSCTF_IMAGE'], IMAGE)
            with patch.object(manage, 'run') as run:
                manage.update(self.installation)
                self.assertIn('--state-file', run.call_args.args[0])
            backup.assert_not_called()
        self.assertEqual(self.installation.events, [])
        self.assertFalse(state_path.exists())
        self.assertEqual(manage.read_env(self.installation.env_path)['RSCTF_IMAGE'], image)

    def test_schedule_preserves_other_jobs_and_replaces_own_job(self):
        existing = '15 2 * * * unrelated-command\n'
        saved = []
        def write(args, **kwargs):
            saved.append(kwargs['input'].decode())
        with patch.object(manage.shutil, 'which', return_value='/usr/bin/crontab'), patch.object(manage, 'run', side_effect=write):
            with patch.object(manage.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, existing, '')):
                manage.schedule(self.installation, True, '03:00')
            self.assertTrue(saved[-1].startswith(existing))
            self.assertIn('backup --keep 7', saved[-1])
            with patch.object(manage.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, saved[-1], '')):
                manage.schedule(self.installation, False, '03:00')
            self.assertEqual(saved[-1], existing)


@unittest.skipUnless(os.environ.get('RSCTF_MANAGE_DOCKER_TEST') == '1', 'opt-in isolated Docker/PostgreSQL drill')
class DockerRestore(unittest.TestCase):
    def test_database_files_ownership_and_new_project_restore(self):
        with tempfile.TemporaryDirectory(prefix='rsctf-manage-docker-') as temporary:
            root = Path(temporary) / 'installation'
            deploy = root / 'deploy'
            deploy.mkdir(parents=True)
            project = 'rsctf-maintenance-test-' + manage.uuid.uuid4().hex[:10]
            # Real PostgreSQL and file volume; a sleeping app fixture isolates the
            # backup lifecycle from this host's missing IPv6 REJECT kernel module.
            compose = {'services': {
                'db': {'image': 'postgres:18.4-alpine3.24', 'environment': {'POSTGRES_USER': 'rsctf', 'POSTGRES_DB': 'rsctf', 'POSTGRES_PASSWORD': 'disposable-fixture'},
                       'healthcheck': {'test': ['CMD', 'pg_isready', '-h', '127.0.0.1', '-U', 'rsctf'], 'interval': '1s', 'retries': 30}, 'volumes': ['postgres_data:/var/lib/postgresql']},
                'redis': {'image': 'redis:7-alpine'},
                'rsctf': {'image': '${RSCTF_IMAGE}', 'init': True, 'restart': 'unless-stopped', 'healthcheck': {'test': ['CMD', 'python3', '-c', 'pass'], 'interval': '1s'}, 'entrypoint': ['python3', '-c', 'import time; time.sleep(3600)'],
                          'environment': {'RSCTF_ROLE': 'all', 'RSCTF_STORAGE_BACKEND': 'local', 'RSCTF_CONTAINER_BACKEND': 'none'}, 'volumes': ['files_data:/data/files']}},
                'volumes': {'postgres_data': {}, 'files_data': {}}}
            (deploy / 'compose.yml').write_text(json.dumps(compose))
            (deploy / 'release.env').write_text(f'RSCTF_RELEASE_VERSION=v0.1.156\nRSCTF_RELEASE_IMAGE={IMAGE}\n')
            (deploy / '.env').write_text(f'COMPOSE_PROJECT_NAME={project}\nCOMPOSE_FILE=compose.yml\nRSCTF_IMAGE={IMAGE}\n')
            original = manage.Installation(root)
            restored = None
            try:
                startup = subprocess.run(original.command + ['up', '-d', '--wait'], cwd=original.deploy, env=original.environment, capture_output=True, text=True)
                if startup.returncode:
                    self.fail(startup.stderr + original.read('logs', '--tail', '30', 'db', 'rsctf'))
                original.call('exec', '-T', 'db', 'psql', '-U', 'rsctf', '-c', "CREATE TABLE evidence(value text); INSERT INTO evidence VALUES ('survives restore');", stdout=subprocess.DEVNULL)
                original.call('exec', '-T', 'rsctf', 'python3', '-c', "import os; p='/data/files/evidence.txt'; open(p,'w').write('uploaded evidence'); os.chown(p,1234,2345); os.chmod(p,0o640)")
                saved = manage.backup(original)
                with tarfile.open(saved / 'files.tar') as handle:
                    evidence = next(member for member in handle if member.name.endswith('evidence.txt'))
                    self.assertEqual((evidence.uid, evidence.gid), (1234, 2345))
                self.assertTrue(original.runtime()['State']['Running'])
                destination = Path(temporary) / 'recovered'
                def fixture_download(target, version=None):
                    shutil.copytree(root / 'deploy', target / 'deploy', ignore=shutil.ignore_patterns('.env'))
                with patch.object(manage, 'download', side_effect=fixture_download):
                    manage.restore(saved, destination, 8081)
                restored = manage.Installation(destination)
                self.assertNotEqual(restored.project, original.project)
                result = restored.read('exec', '-T', 'db', 'psql', '-U', 'rsctf', '-Atc', 'SELECT value FROM evidence')
                self.assertEqual(result.strip(), 'survives restore')
                result = restored.read('exec', '-T', 'rsctf', 'python3', '-c', "import os,json; p='/data/files/evidence.txt'; s=os.stat(p); print(json.dumps([open(p).read(),s.st_uid,s.st_gid,s.st_mode & 0o777]))")
                self.assertEqual(json.loads(result), ['uploaded evidence', 1234, 2345, 0o640])
                self.assertTrue(original.runtime()['State']['Running'])
            finally:
                if restored: restored.call('down', '--volumes', stdout=subprocess.DEVNULL)
                else:
                    destination = Path(temporary) / 'recovered'
                    if (destination / 'deploy/.env').exists():
                        manage.Installation(destination).call('down', '--volumes', stdout=subprocess.DEVNULL)
                original.call('down', '--volumes', stdout=subprocess.DEVNULL)


if __name__ == '__main__':
    os.umask(0o077)
    unittest.main()
