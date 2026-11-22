"""Pin RSCTF patch images before offline cleanup of a dedicated event registry."""

import argparse
import hashlib
from itertools import groupby, zip_longest
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

import psycopg

from registry import registry_writer, service_repository, skopeo


# Scan all teams and challenges. An organizer can reuse a digest belonging to
# another service. Even an interrupted reset with a blank endpoint owns its pins.
ROOTS = '''
    WITH refs(image) AS (
        SELECT registry_image_ref FROM "AdPatchCandidates"
        UNION ALL SELECT runtime_image_ref FROM "AdPatchStarters"
        UNION ALL SELECT value FROM "GameChallenges" c CROSS JOIN LATERAL
          (VALUES (c.build_image_digest), (c.container_image), (c.ad_checker_image),
                  (c.variant_generator_image), (c.variant_generator_digest)) AS pins(value)
        UNION ALL SELECT (s->'image'->>'repository') || '@' || (s->'image'->>'digest')
          FROM "GameChallenges" c CROSS JOIN LATERAL jsonb_array_elements(c.workload_spec->'services') s
          WHERE s->'image'->>'type'='registryDigest'
        UNION ALL SELECT container_image FROM "ExerciseChallenges"
        UNION ALL SELECT published_image_ref FROM "AdTeamServices"
        UNION ALL SELECT previous_image_ref FROM "AdTeamServices"
        UNION ALL SELECT value FROM "ControlPlaneJobs" j
          CROSS JOIN LATERAL (VALUES (j.input->>'candidateImageRef'),
              (j.input->>'preparedImageRef'), (j.input->>'previousImageRef')) AS pins(value)
          WHERE j.kind IN ('AdReset','AdRollback','AdActivate') AND j.status IN (0,1)
    )
    SELECT DISTINCT btrim(image) COLLATE "C" AS ref FROM refs
    WHERE starts_with(btrim(image), %s) ORDER BY ref
'''
SLOT = re.compile(r'rsctf-retain-([1-9][0-9]*)\Z')


def snapshot(db, prefix, output):
    """Spool one statement snapshot without holding a transaction during registry I/O."""
    with db.transaction(), db.cursor(name='patch_retention') as cursor:
        cursor.itersize = 128
        cursor.execute(ROOTS, (prefix.partition('/')[0] + '/',))
        for (ref,) in cursor:
            if not ref.startswith(prefix + '/'):
                raise ValueError('this registry host also serves database references outside the patch namespace')
            tail = ref[len(prefix) + 1:]
            match = re.fullmatch(r'service-([1-9][0-9]{0,9})@(sha256:[0-9a-f]{64})', tail)
            if not match or len(ref) > 512:
                raise ValueError('the patch namespace contains a database reference outside the service digest format')
            repository = service_repository(prefix, int(match[1]))
            output.write(f'{repository}@{match[2]}\n')
    output.seek(0)


def registry_output(args):
    with tempfile.TemporaryFile() as output:
        skopeo(args, stdout=output)
        output.seek(0)
        data = output.read(1024 * 1024 + 1)
    if len(data) > 1024 * 1024:
        raise ValueError('registry metadata exceeds the retention limit')
    return data


def pin_repository(repository, references, authfile, cert_dir):
    source = f'docker://{repository}'
    options = ['--tls-verify=true']
    copy_options = ['--src-tls-verify=true', '--dest-tls-verify=true']
    for option, value in [('authfile', authfile), ('cert-dir', cert_dir)]:
        if value:
            options += [f'--{option}', str(value)]
            copy_options += [f'--src-{option}', str(value), f'--dest-{option}', str(value)]
    def inspect(tag):
        raw = registry_output(['inspect', '--raw', *options, f'{source}:{tag}'])
        return 'sha256:' + hashlib.sha256(raw).hexdigest()

    def write(tag, digest):
        skopeo(['copy', '--all', '--preserve-digests', *copy_options,
                f'{source}@{digest}', f'{source}:{tag}'], stdout=subprocess.DEVNULL)
        if inspect(tag) != digest:
            raise ValueError('registry retention tag differs from the pinned manifest')

    needed = set(references)
    list_tags = ['list-tags', *options, source]
    try:
        data = registry_output(list_tags)
    except subprocess.CalledProcessError:
        # Distribution returns NAME_UNKNOWN for a digest-only repository
        # until its first tag is written. Require a verified copy and a
        # successful second listing; a transport error is never an empty list.
        write('rsctf-retain-initial', min(needed))
        data = registry_output(list_tags)
    listing = json.loads(data)
    if not isinstance(listing, dict) or 'Tags' not in listing:
        raise ValueError('registry did not return a tag listing')
    tags = listing['Tags']
    if tags is None:
        tags = []
    if not isinstance(tags, list) or not all(isinstance(tag, str) for tag in tags):
        raise ValueError('invalid tag list from registry')
    slots = {tag: inspect(tag) for tag in tags if SLOT.fullmatch(tag)}
    # Keep one existing alias per required digest. Never overwrite its last
    # alias to make room for another version, even if the next copy fails.
    protected = set()
    kept = set()
    for tag in sorted(slots):
        digest = slots[tag]
        if digest in needed and digest not in kept:
            protected.add(tag)
            kept.add(digest)
    reusable = sorted(slots.keys() - protected)
    next_index = 1

    for digest in sorted(needed - kept):
        if reusable:
            tag = reusable.pop()
        else:
            while f'rsctf-retain-{next_index}' in slots:
                next_index += 1
            tag = f'rsctf-retain-{next_index}'
            next_index += 1
        write(tag, digest)
    # Tags cannot be deleted portably without deleting their manifest. Point
    # surplus slots at a retained image so they no longer keep old layers alive.
    fallback = min(needed)
    for tag in reusable:
        if slots[tag] not in needed:
            write(tag, fallback)
    # The bootstrap alias may need to be rewritten if tag listing ever fails
    # again. Never rely on it as the only alias of a required manifest.
    if 'rsctf-retain-initial' in tags and inspect('rsctf-retain-initial') not in needed:
        write('rsctf-retain-initial', fallback)
    return len(needed)


def pin_candidate(db, repository, digest, authfile, cert_dir):
    """The staging command holds the service writer lock until receipt publication."""
    def required():
        prefix = repository + '@'
        digests = {ref[len(prefix):] for (ref,) in db.execute(ROOTS, (prefix,))}
        if any(not re.fullmatch(r'sha256:[0-9a-f]{64}', value) for value in digests):
            raise ValueError('invalid registry digest in patch retention references')
        return digests | {digest}

    before = required()
    pin_repository(repository, before, authfile, cert_dir)
    if not required() <= before:
        raise ValueError('new image references appeared during staging; retry to pin them')


def pin(db, prefix, authfile=None, cert_dir=None):
    service_repository(prefix, 2147483647)
    if not db.autocommit:
        raise ValueError('retention requires an autocommit database connection')
    count = 0
    with tempfile.TemporaryFile(mode='w+t') as before, tempfile.TemporaryFile(mode='w+t') as after:
        snapshot(db, prefix, before)
        if not before.readline():
            raise ValueError('no registry references found; check the database and the dedicated namespace')
        before.seek(0)
        refs = (line.rstrip('\n').rsplit('@', 1) for line in before)
        for repository, rows in groupby(refs, key=lambda row: row[0]):
            service = int(repository.rsplit('/service-', 1)[1])
            with registry_writer(db, service):
                count += pin_repository(repository, (row[1] for row in rows), authfile, cert_dir)
        snapshot(db, prefix, after)
        before.seek(0)
        if any(first != second for first, second in zip_longest(before, after)):
            raise ValueError('registry image references changed; pause writers and repeat before collection')
    return count


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', required=True, help='dedicated registry host and event namespace')
    parser.add_argument('--authfile', type=Path)
    parser.add_argument('--cert-dir', type=Path)
    args = parser.parse_args()
    try:
        with psycopg.connect(os.environ['RSCTF_DATABASE_URL'], autocommit=True) as db:
            count = pin(db, args.repository, args.authfile, args.cert_dir)
    except psycopg.Error as error:
        sys.exit(f'could not read patch references (PostgreSQL {error.sqlstate or "connection error"})')
    except subprocess.SubprocessError:
        sys.exit('could not pin a manifest; check registry access, TLS and the skopeo source policy')
    except (KeyError, OSError, ValueError) as error:
        sys.exit(str(error))
    print(f'Verified retention tags for {count} distinct registry image references.')


if __name__ == '__main__':
    main()
