"""Verify maintenance against the TLS registry used for private patch staging."""

import gzip
import hashlib
import io
import json
from pathlib import Path
import ssl
import subprocess
import tarfile
import tempfile
from unittest.mock import patch
from urllib.error import HTTPError
from urllib.request import Request, urlopen

from psycopg.types.json import Jsonb
import retain
from retain import pin, pin_repository
from stage import skopeo


def put_versions(archive, root, repository, auth, trust):
    layout = root / 'versions'
    skopeo(['copy', '--preserve-digests', f'oci-archive:{archive}', f'oci:{layout}:test'],
           stdout=subprocess.DEVNULL)
    index = json.loads((layout / 'index.json').read_bytes())
    blobs = layout / 'blobs' / 'sha256'

    def read(descriptor):
        return json.loads((blobs / descriptor['digest'].split(':')[1]).read_bytes())

    def blob(data, media):
        identifier = hashlib.sha256(data).hexdigest()
        (blobs / identifier).write_bytes(data)
        return {'digest': 'sha256:' + identifier, 'size': len(data), 'mediaType': media}

    descriptor = index['manifests'][0]
    original = read(descriptor)
    refs = {}
    for name in ('obsolete', 'active', 'previous', 'starter', 'reviewed', 'pending', 'peer', 'abandoned'):
        manifest = dict(original)
        config = read(original['config'])
        layer = io.BytesIO()
        with tarfile.open(fileobj=layer, mode='w') as tar:
            data = name.encode()
            entry = tarfile.TarInfo('retention-fixture')
            entry.size = len(data)
            tar.addfile(entry, io.BytesIO(data))
        raw = layer.getvalue()
        config['rootfs']['diff_ids'].append('sha256:' + hashlib.sha256(raw).hexdigest())
        manifest['layers'] = original['layers'] + [blob(gzip.compress(raw, mtime=0),
                                                   'application/vnd.oci.image.layer.v1.tar+gzip')]
        manifest['config'] = blob(json.dumps(config).encode(), original['config']['mediaType'])
        desc = blob(json.dumps(manifest).encode(), descriptor['mediaType'])
        desc['annotations'] = {'org.opencontainers.image.ref.name': 'test'}
        index['manifests'] = [desc]
        (layout / 'index.json').write_text(json.dumps(index))
        repo = repository.rsplit('/', 1)[0] + '/service-2' if name == 'peer' else repository
        ref = repo + '@' + desc['digest']
        skopeo(['copy', '--preserve-digests', '--dest-cert-dir', str(trust), '--dest-authfile', str(auth),
                f'oci:{layout}:test', 'docker://' + ref], stdout=subprocess.DEVNULL)
        refs[name] = ref
    return refs


def create_tables(db):
    db.execute('''CREATE TABLE "AdPatchStarters"(runtime_image_ref text);
        CREATE TABLE "GameChallenges"(build_image_digest text,container_image text,ad_checker_image text,
                                      variant_generator_image text,variant_generator_digest text,workload_spec jsonb);
        CREATE TABLE "ExerciseChallenges"(container_image text);
        CREATE TABLE "AdTeamServices"(published_image_ref text,previous_image_ref text);
        CREATE TABLE "ControlPlaneJobs"(kind text,status int,input jsonb)''')


def prepare(fixture, root, archive, ref, auth, trust):
    db = fixture.db
    repo = ref.split('@')[0]
    prefix = repo.rsplit('/', 1)[0]
    refs = put_versions(archive, root, repo, auth, trust)
    db.execute('INSERT INTO "AdPatchStarters" VALUES (%s)', (refs['starter'],))
    db.execute('INSERT INTO "GameChallenges"(build_image_digest) VALUES (%s)', (refs['reviewed'],))
    db.execute('INSERT INTO "AdTeamServices" VALUES (%s,%s),(%s,NULL)',
               (refs['active'], refs['obsolete'], refs['peer']))
    db.execute('INSERT INTO "ControlPlaneJobs" VALUES (%s,1,%s),(%s,2,%s)', (
        'AdRollback', Jsonb({'previousImageRef': refs['pending']}), 'AdActivate',
        Jsonb({'preparedImageRef': refs['obsolete']})))
    db.execute('INSERT INTO "ControlPlaneJobs" VALUES (%s,0,%s)',
               ('AdActivate', Jsonb({'candidateImageRef': refs['abandoned']})))

    def sync():
        return pin(db, prefix, auth, trust)

    with fixture.assertRaisesRegex(ValueError, 'outside the patch namespace'):
        pin(db, prefix + '-wrong', auth, trust)
    with fixture.assertRaisesRegex(ValueError, 'no registry references'):
        pin(db, 'registry.invalid/other', auth, trust)
    fixture.other.execute('SELECT pg_advisory_lock(1734271013,1)')
    try:
        with fixture.assertRaisesRegex(ValueError, 'transfer'):
            sync()
    finally:
        fixture.other.execute('SELECT pg_advisory_unlock(1734271013,1)')

    def rotate(*args):
        fixture.other.execute('UPDATE "AdTeamServices" SET previous_image_ref=%s WHERE previous_image_ref=%s',
                              (refs['previous'], refs['obsolete']))
        return pin_repository(*args)

    with patch('retain.pin_repository', side_effect=rotate), fixture.assertRaisesRegex(ValueError, 'changed'):
        sync()
    assert sync() == 8
    db.execute('UPDATE "ControlPlaneJobs" SET status=3 WHERE status=0')
    assert sync() == 7
    # No registry copies should be necessary for a stable snapshot.
    with patch('retain.skopeo', wraps=retain.skopeo) as client:
        assert sync() == 7
        assert all(call.args[0][0] != 'copy' for call in client.call_args_list)
    return ([ref, *(v for k, v in refs.items() if k not in ('obsolete', 'abandoned'))],
            [refs['obsolete'], refs['abandoned']])


def collect(config, storage):
    def blob_bytes():
        return sum(f.stat().st_size for f in storage.rglob('data'))

    before = blob_bytes()
    subprocess.run(['docker-registry', 'garbage-collect', '--delete-untagged', config],
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120)
    reclaimed = before - blob_bytes()
    assert reclaimed > 0, 'offline collection reclaimed no blobs'
    print(f'Offline registry collection reclaimed {reclaimed} blob bytes.')


def check_pulls(live, obsolete, auth, trust):
    def pull(ref, directory):
        skopeo(['copy', '--preserve-digests', '--src-authfile', str(auth), '--src-cert-dir', str(trust),
                'docker://' + ref, 'dir:' + directory], stdout=subprocess.DEVNULL)

    for ref in live:
        with tempfile.TemporaryDirectory() as directory:
            pull(ref, directory)
            assert ref.endswith('@sha256:' + hashlib.sha256((Path(directory) / 'manifest.json').read_bytes()).hexdigest())
    context = ssl.create_default_context(cafile=str(trust / 'ca.crt'))
    accounts = json.loads(auth.read_text())['auths']
    for ref in obsolete:
        host, path = ref.split('/', 1)
        repository, digest = path.rsplit('@', 1)
        request = Request(f'https://{host}/v2/{repository}/manifests/{digest}', headers={
            'Authorization': 'Basic ' + accounts[host]['auth'],
            'Accept': 'application/vnd.oci.image.manifest.v1+json',
        })
        try:
            with urlopen(request, context=context, timeout=10):
                raise AssertionError('obsolete manifest was still present after collection')
        except HTTPError as error:
            with error:
                assert error.code == 404
                assert 'MANIFEST_UNKNOWN' in {item['code'] for item in json.load(error)['errors']}
