#!/usr/bin/env python3
"""Small, fail-closed maintenance commands for the guided single-server install."""

import argparse
import datetime as dt
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path, PurePosixPath
import re
import shlex
import shutil
import subprocess
import sys
import tarfile
import uuid

ROOT = Path(__file__).resolve().parent.parent
DIGEST = re.compile(r"ghcr\.io/dimasma0305/rsctf@sha256:[0-9a-f]{64}")
TAG = re.compile(r"v\d+\.\d+\.\d+")


def fail(message):
    raise RuntimeError(message)


def run(args, **kwargs):
    # Capture errors: Compose config and database errors can contain credentials.
    result = subprocess.run([str(arg) for arg in args], stderr=subprocess.PIPE, **kwargs)
    if result.returncode:
        fail(f"{Path(str(args[0])).name} could not finish. Run ./rsctf doctor and check Docker, then retry the command.")
    return result


def output(args, **kwargs):
    return run(args, stdout=subprocess.PIPE, **kwargs).stdout.decode()


def read_env(path):
    values = {}
    for line in path.read_text().splitlines():
        if not line or line.startswith('#'):
            continue
        key, separator, value = line.partition('=')
        if not separator or key in values or not re.fullmatch(r'[A-Z][A-Z0-9_]*', key):
            fail('This command needs an installer-generated .env without duplicate keys. See the advanced operations guide.')
        values[key] = value
    return values


def write_env(path, values):
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    with temporary.open('x') as handle:
        os.chmod(temporary, 0o600)
        handle.write(''.join(f'{key}={value}\n' for key, value in values.items()))
        handle.flush()
        os.fsync(handle.fileno())
    temporary.replace(path)


def private_json(path, value):
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    with temporary.open('x') as handle:
        os.chmod(temporary, 0o600)
        json.dump(value, handle)
        handle.flush()
        os.fsync(handle.fileno())
    temporary.replace(path)


class Installation:
    def __init__(self, root):
        self.root = root.resolve()
        self.deploy = self.root / 'deploy'
        self.env_path = self.deploy / '.env'
        if not self.env_path.is_file():
            fail('No installation settings found. Run scripts/install.sh first.')
        self.values = read_env(self.env_path)
        self.environment = {key: value for key, value in os.environ.items() if not key.startswith(('RSCTF_', 'POSTGRES_', 'REDIS_', 'COMPOSE_'))}
        self.environment.update(self.values)
        # The install's .env, rather than caller Compose selectors, owns this project.
        for key in ('COMPOSE_FILE', 'COMPOSE_PROJECT_NAME', 'COMPOSE_PROFILES', 'COMPOSE_ENV_FILES'):
            self.environment.pop(key, None)
        self.project = self.values.get('COMPOSE_PROJECT_NAME', 'rsctf')
        if not re.fullmatch(r'[a-z0-9][a-z0-9_-]*', self.project):
            fail('The Compose project name is invalid.')
        files = self.values.get('COMPOSE_FILE', 'compose.yml').split(':')
        if any(name not in ('compose.yml', 'compose.caddy.yml') for name in files):
            fail('Use the advanced operations guide for split roles, container challenges, or VPN installations.')
        self.command = ['docker', 'compose', '--project-directory', self.deploy, '--env-file', self.env_path, '--project-name', self.project]
        for name in files:
            self.command += ['-f', self.deploy / name]
        self.config = json.loads(self.read('config', '--format', 'json'))
        service = self.config['services']['rsctf']
        environment = service.get('environment', {})
        if (environment.get('RSCTF_ROLE') != 'all' or environment.get('RSCTF_STORAGE_BACKEND', 'local') != 'local'
                or environment.get('RSCTF_CONTAINER_BACKEND', 'none') != 'none'
                or environment.get('RSCTF_RUNTIME_PROFILES', '[]') != '[]'):
            fail('Automatic maintenance supports the basic, local-storage install. Use the advanced operations guide for this configuration.')
        self.image = service['image']
        if not DIGEST.fullmatch(self.image):
            fail('Pin an official immutable release image before maintenance.')
        self.volume = next((mount['source'] for mount in service.get('volumes', []) if mount.get('target') == '/data/files' and mount['type'] == 'volume'), None)
        if not self.volume:
            fail('The uploaded-file volume could not be identified.')
        self.volume = self.config['volumes'][self.volume]['name']

    def call(self, *args, **kwargs):
        return run(self.command + list(args), cwd=self.deploy, env=self.environment, **kwargs)

    def read(self, *args):
        return self.call(*args, stdout=subprocess.PIPE).stdout.decode()

    def runtime(self):
        ids = output(['docker', 'ps', '-aq', '--filter', f'label=com.docker.compose.project={self.project}']).split()
        entries = json.loads(output(['docker', 'inspect', *ids])) if ids else []
        runtime = []
        for entry in entries:
            labels = entry['Config'].get('Labels') or {}
            service = labels.get('com.docker.compose.service')
            if service not in self.config['services'] or labels.get('com.docker.compose.oneoff', 'False') != 'False':
                fail('An extra project container exists. Finish or remove that operation before maintenance.')
            if service == 'rsctf':
                if entry['HostConfig']['RestartPolicy']['Name'] != 'unless-stopped':
                    fail('The app must use restart: unless-stopped before maintenance.')
                if entry['Config']['Image'] != self.image:
                    fail('The running image differs from .env. Finish the previous update first.')
                runtime.append(entry)
        if len(runtime) != 1:
            fail('Expected one RSCTF application container. Check ./rsctf status.')
        return runtime[0]

    def copy_files(self, destination, restore=False):
        if restore:
            validate_files_archive(destination)
            # Docker cp from stdin rewrites ownership even with -a. Extract in
            # the isolated volume with only the capabilities ownership needs.
            with destination.open('rb') as handle:
                run(['docker', 'run', '--rm', '-i', '--network', 'none', '--read-only',
                     '--cap-drop', 'ALL', '--cap-add', 'CHOWN', '--cap-add', 'FOWNER',
                     '--cap-add', 'DAC_OVERRIDE', '--security-opt', 'no-new-privileges',
                     '--user', '0', '--entrypoint', 'tar',
                     '--mount', f'type=volume,src={self.volume},dst=/data', self.image,
                     '--extract', '--preserve-permissions', '--numeric-owner', '--file=-', '--directory=/data'], stdin=handle)
            return
        container = 'rsctf-files-' + uuid.uuid4().hex
        args = ['docker', 'create', '--name', container, '--network', 'none', '--read-only', '--cap-drop', 'ALL',
                '--security-opt', 'no-new-privileges', '--user', '0', '--entrypoint', 'true',
                '--mount', f'type=volume,src={self.volume},dst=/data' + ('' if restore else ',readonly'), self.image]
        run(args, stdout=subprocess.DEVNULL)
        try:
            with destination.open('xb') as handle:
                run(['docker', 'cp', '-a', container + ':/data/.', '-'], stdout=handle)
            validate_files_archive(destination)
        finally:
            run(['docker', 'rm', container], stdout=subprocess.DEVNULL)


def validate_files_archive(path):
    with tarfile.open(path, mode='r|') as archive:
        for member in archive:
            name = PurePosixPath(member.name)
            if name.is_absolute() or '..' in name.parts or not (member.isfile() or member.isdir() or member.issym() or member.islnk()):
                fail('The uploaded-file archive contains an unsafe path or special file.')
            if member.issym() or member.islnk():
                target = PurePosixPath(member.linkname)
                if target.is_absolute() or '..' in target.parts:
                    fail('The uploaded-file archive contains an unsafe link. Use the advanced backup guide.')


def file_inventory(directory):
    inventory = {}
    for path in sorted(directory.rglob('*')):
        relative = path.relative_to(directory).as_posix()
        if path.is_symlink():
            # Refuse links outside the saved tree, including absolute links.
            target = os.readlink(path)
            if Path(target).is_absolute() or not path.resolve().is_relative_to(directory.resolve()):
                fail('The backup contains a link outside its files. Use the advanced backup guide.')
            inventory[relative] = {'link': target}
        elif path.is_file():
            checksum = hashlib.sha256()
            with path.open('rb') as handle:
                for chunk in iter(lambda: handle.read(1024 * 1024), b''):
                    checksum.update(chunk)
            digest = checksum.hexdigest()
            inventory[relative] = {'sha256': digest, 'size': path.stat().st_size}
        elif path.is_dir():
            inventory[relative] = {'directory': True}
        else:
            fail('The backup contains an unsupported special file.')
    return inventory


def backup(installation, resume=True, updating=False):
    if (installation.root / '.rsctf-update.json').exists() and not updating:
        fail('Finish the interrupted update before making another backup.')
    entry = installation.runtime()
    if not entry['State']['Running']:
        fail('The app is already stopped. Check its status before making a backup.')
    directory = installation.root / 'backups'
    directory.mkdir(mode=0o700, exist_ok=True)
    name = dt.datetime.now(dt.timezone.utc).strftime('%Y%m%dT%H%M%SZ') + '-' + uuid.uuid4().hex[:8]
    pending = directory / ('.partial-' + name)
    pending.mkdir(mode=0o700)
    print('Pausing the game while saving the database and uploaded files…', flush=True)
    installation.call('stop', '--timeout', '300', 'rsctf', stdout=subprocess.DEVNULL)
    completed = False
    try:
        if installation.runtime()['State']['Running']:
            fail('The app did not stop. No backup was taken.')
        with (pending / 'database.dump').open('xb') as handle:
            installation.call('exec', '-T', 'db', 'sh', '-c', 'exec pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc', stdout=handle)
        installation.copy_files(pending / 'files.tar')
        shutil.copyfile(installation.env_path, pending / 'settings.env')
        os.chmod(pending / 'settings.env', 0o600)
        release = read_env(installation.deploy / 'release.env')
        inventory = file_inventory(pending)
        private_json(pending / 'manifest.json', {'format': 1, 'version': release['RSCTF_RELEASE_VERSION'], 'image': installation.image, 'inventory': inventory})
        final = directory / name
        pending.rename(final)
        completed = True
        print(f'Backup saved: {final}\nKeep a copy on another disk. It contains account secrets.', flush=True)
        return final
    finally:
        # A failed snapshot has not changed schema; resuming the same binary is safe.
        if resume or not completed:
            installation.call('start', 'rsctf', stdout=subprocess.DEVNULL)


def verify_backup(path):
    if path.is_symlink() or not path.is_dir():
        fail('Choose a backup directory created by ./rsctf backup.')
    manifest = json.loads((path / 'manifest.json').read_text())
    if manifest.get('format') != 1 or not TAG.fullmatch(manifest.get('version', '')) or not DIGEST.fullmatch(manifest.get('image', '')):
        fail('This backup has an unsupported manifest.')
    actual = file_inventory(path)
    actual.pop('manifest.json', None)
    if actual != manifest.get('inventory') or not {'database.dump', 'settings.env', 'files.tar'} <= actual.keys():
        fail('Backup verification failed. A file is missing or changed; use another backup.')
    validate_files_archive(path / 'files.tar')
    return manifest


def download(destination, version=None):
    args = ['bash', ROOT / 'scripts/install.sh', '--download-only', '--install-dir', destination]
    if version:
        args += ['--ref', version]
    environment = dict(os.environ)
    for key in ('RSCTF_REF', 'RSCTF_IMAGE', 'RSCTF_BUNDLE_URL', 'RSCTF_INSTALL_DIR'):
        environment.pop(key, None)
    run(args, env=environment)


def update(installation):
    state_path = installation.root / '.rsctf-update.json'
    if state_path.exists():
        state = json.loads(state_path.read_text())
        candidate = Path(state['candidate'])
        image = state['image']
        if not candidate.resolve().is_relative_to(installation.root / '.releases') or not DIGEST.fullmatch(image):
            fail('Invalid pending update. See the operations guide before continuing.')
        print('Resuming the interrupted update…', flush=True)
    else:
        releases = installation.root / '.releases'
        releases.mkdir(mode=0o700, exist_ok=True)
        candidate = releases / uuid.uuid4().hex
        download(candidate)
        metadata = read_env(candidate / 'deploy/release.env')
        image = metadata['RSCTF_RELEASE_IMAGE']
        if not DIGEST.fullmatch(image):
            fail('The verified release did not provide an immutable image.')
        if image == installation.image:
            print('You already have the latest release.')
            return
        run(['docker', 'pull', image], stdout=subprocess.DEVNULL)
        state = {'candidate': str(candidate), 'image': image, 'phase': 'backup', 'sourceImage': installation.image}
        private_json(state_path, state)
    if state.get('phase') == 'backup':
        if state['sourceImage'] != installation.image:
            fail('The installation image changed during backup. Check the pending update before continuing.')
        if not installation.runtime()['State']['Running']:
            # This durable phase precedes every schema change. Resume only this exact source image.
            installation.call('start', 'rsctf', stdout=subprocess.DEVNULL)
        saved = backup(installation, updating=True)
        state.update(phase='cutover', backup=str(saved))
        private_json(state_path, state)
    values = {**installation.values, 'RSCTF_IMAGE': image}
    candidate_env = candidate / 'deploy/.env'
    if not candidate_env.exists():
        write_env(candidate_env, values)
    next_install = Installation(candidate)
    # Preserve the established migration/drain/replica verification contract.
    command = ['bash', candidate / 'scripts/compose-maintenance-cutover.sh', '--project-name', installation.project,
               '--project-directory', candidate / 'deploy', '--env-file', candidate_env, '--image', image,
               '--state-file', installation.deploy / '.easy-cutover.json']
    for name in values.get('COMPOSE_FILE', 'compose.yml').split(':'):
        command += ['--compose-file', candidate / 'deploy' / name]
    run(command, env=next_install.environment, cwd=candidate / 'deploy')
    for source in (candidate / 'deploy').rglob('*'):
        if source.is_file() and source.name != '.env':
            target = installation.deploy / source.relative_to(candidate / 'deploy')
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
    write_env(installation.env_path, values)
    for source in (candidate / 'scripts').iterdir():
        if source.is_file():
            shutil.copy2(source, installation.root / 'scripts' / source.name)
    shutil.copy2(candidate / 'rsctf', installation.root / 'rsctf')
    state_path.unlink()
    print('Update complete. Your settings and accounts are unchanged.')


def restore(path, destination, port):
    manifest = verify_backup(path)
    if destination.exists():
        fail('Choose a new folder with --to. Restore never overwrites an existing installation.')
    print(f'Restoring a separate copy at http://localhost:{port}. The current installation stays unchanged.', flush=True)
    download(destination, manifest['version'])
    released = read_env(destination / 'deploy/release.env')
    if released['RSCTF_RELEASE_IMAGE'] != manifest['image']:
        fail('The backup image does not match its verified release.')
    values = read_env(path / 'settings.env')
    network_ids = output(['docker', 'network', 'ls', '-q']).split()
    networks = json.loads(output(['docker', 'network', 'inspect', *network_ids])) if network_ids else []
    used = [ipaddress.ip_network(config['Subnet']) for network in networks for config in (network.get('IPAM', {}).get('Config') or []) if config.get('Subnet')]
    subnet = next((candidate for candidate in ipaddress.ip_network('172.16.0.0/12').subnets(new_prefix=24)
                   if all(candidate.version != item.version or not candidate.overlaps(item) for item in used)), None)
    if subnet is None:
        fail('No free private Docker subnet was found. Ask your server administrator to choose one.')
    values.update(COMPOSE_PROJECT_NAME='rsctf-restore-' + uuid.uuid4().hex[:12], COMPOSE_FILE='compose.yml',
                  RSCTF_IMAGE=manifest['image'], RSCTF_HTTP_BIND_IP='127.0.0.1', RSCTF_HTTP_PORT=str(port),
                  RSCTF_PUBLIC_URL=f'http://localhost:{port}', RSCTF_COOKIE_SECURE='false', RSCTF_TRUSTED_PROXY_CIDRS='',
                  RSCTF_CHALLENGE_PROXY_SUBNET=str(subnet), RSCTF_DOCKER_PROXY_BIND=str(subnet.network_address + 1),
                  RSCTF_CHALLENGE_PROXY_BRIDGE='rc-' + uuid.uuid4().hex[:10])
    write_env(destination / 'deploy/.env', values)
    installation = Installation(destination)
    try:
        installation.call('up', '-d', '--wait', 'db', 'redis', stdout=subprocess.DEVNULL)
        installation.call('create', 'rsctf', stdout=subprocess.DEVNULL)
        with (path / 'database.dump').open('rb') as handle:
            installation.call('exec', '-T', 'db', 'sh', '-c', 'exec pg_restore --exit-on-error --no-owner -U "$POSTGRES_USER" -d "$POSTGRES_DB"', stdin=handle, stdout=subprocess.DEVNULL)
        installation.copy_files(path / 'files.tar', restore=True)
        installation.call('up', '-d', '--wait', '--wait-timeout', '300', stdout=subprocess.DEVNULL)
    except (RuntimeError, OSError, KeyboardInterrupt):
        # Keep the recovered data for inspection, but do not leave a partial
        # application running. This command targets only the new project.
        installation.call('stop', '--timeout', '30', stdout=subprocess.DEVNULL)
        fail(f'Restore did not finish. Its separate project {installation.project} is stopped; files remain in {destination}. Check Docker and retry with a new --to folder. Your original installation and backup are unchanged.')
    print(f'Restore complete. Open http://localhost:{port} and sign in with your existing account. Check a game and an uploaded file before using this copy.')


def schedule(installation, enabled, time):
    if not shutil.which('crontab'):
        fail('Install cron first, then rerun ./rsctf backups enable. You can still run ./rsctf backup now.')
    marker = '# rsctf-backup-' + hashlib.sha256(str(installation.root).encode()).hexdigest()[:16]
    result = subprocess.run(['crontab', '-l'], capture_output=True, text=True)
    if result.returncode not in (0, 1) or (result.returncode == 1 and result.stderr and 'no crontab' not in result.stderr.lower()):
        fail('Could not read your backup schedule. Check that cron is available for your user.')
    lines = [line for line in result.stdout.splitlines() if not line.endswith(marker)]
    if enabled:
        hour, minute = map(int, time.split(':'))
        if not (0 <= hour <= 23 and 0 <= minute <= 59):
            fail('Use a time such as 03:00.')
        launcher = shlex.quote(str(installation.root / 'rsctf')).replace('%', r'\%')
        log = shlex.quote(str(installation.root / 'backup.log')).replace('%', r'\%')
        # Shell quoting preserves spaces; cron also treats percent specially.
        if '\n' in launcher or '\r' in launcher:
            fail('The installation path cannot contain a newline.')
        lines.append(f'{minute} {hour} * * * umask 077; {launcher} backup --keep 7 > {log} 2>&1 {marker}')
    run(['crontab', '-'], input=('\n'.join(lines) + '\n').encode())
    print(f'Daily backups enabled at {time} in the host timezone; keeping the newest 7. Games pause briefly. Check backup.log and copy backups to another disk.' if enabled else 'Automatic backups disabled. Existing backups were kept.')


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(prog='./rsctf', description='Keep RSCTF running with simple commands.')
    commands = parser.add_subparsers(dest='command', required=True)
    for name in ('update', 'status', 'logs', 'doctor'):
        commands.add_parser(name)
    snapshot = commands.add_parser('backup')
    snapshot.add_argument('--keep', type=int, help='Keep this many completed backups; default keeps all')
    recovery = commands.add_parser('restore', help='Restore a verified backup into a new, local installation')
    recovery.add_argument('backup', type=Path)
    recovery.add_argument('--to', required=True, type=Path)
    recovery.add_argument('--port', type=int, default=8081)
    automatic = commands.add_parser('backups', help='Schedule daily backups (briefly pauses the game)')
    automatic.add_argument('action', choices=['enable', 'disable'])
    automatic.add_argument('--time', default='03:00')
    args = parser.parse_args()
    if args.command == 'doctor':
        run(['bash', ROOT / 'scripts/install.sh', '--doctor'])
        return
    with (ROOT / '.rsctf-maintenance.lock').open('a') as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            fail('Another backup, restore, or update is running. Try again after it finishes.')
        if args.command == 'restore':
            if not 1 <= args.port <= 65535:
                fail('Choose a port between 1 and 65535.')
            restore(args.backup.resolve(), args.to.resolve(), args.port)
            return
        installation = Installation(ROOT)
        if args.command == 'backup':
            if args.keep is not None and not 1 <= args.keep <= 365:
                fail('--keep must be between 1 and 365.')
            backup(installation)
            if args.keep:
                snapshots = sorted(path for path in (ROOT / 'backups').iterdir()
                                   if re.fullmatch(r'\d{8}T\d{6}Z-[a-f0-9]{8}', path.name)
                                   and path.is_dir() and not path.is_symlink() and (path / 'manifest.json').is_file())
                for path in snapshots[:-args.keep]:
                    verify_backup(path)
                    shutil.rmtree(path)
        elif args.command == 'update':
            update(installation)
        elif args.command == 'backups':
            schedule(installation, args.action == 'enable', args.time)
        else:
            installation.call('ps' if args.command == 'status' else 'logs', *([] if args.command == 'status' else ['--tail', '100', 'rsctf']))


if __name__ == '__main__':
    try:
        main()
    except (RuntimeError, OSError, ValueError, KeyError, tarfile.TarError) as error:
        print(f'RSCTF: {error}', file=sys.stderr)
        sys.exit(1)
