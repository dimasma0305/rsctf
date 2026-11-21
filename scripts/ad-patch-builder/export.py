"""Export one ready, private A&D build from the trusted platform database."""

import argparse
import hashlib
import os
from pathlib import Path
import sys
import uuid

import psycopg

from build import ImageReceipt, MAX_IMAGE_BYTES, private_archive


# A service ID by itself would silently select a teammate's newer build.
READY = '''SELECT c.digest,c.archive_bytes,c.manifest_digest,c.config_digest FROM "AdPatchCandidates" c
    JOIN "AdPatchBuilds" b USING(service_id,revision)
    JOIN "AdPatchBuildEligibleServices" e USING(service_id)
    WHERE c.service_id=%s AND c.revision=%s AND b.attempt=%s AND b.status='Ready' '''


def export_candidate(db, service, revision, attempt, path, challenge=None):
    identity = (service, revision, attempt)
    query = READY
    if challenge is not None:
        query += " AND e.challenge_id=%s"
        identity += (challenge,)
    with db.transaction():
        # Each statement sees a fresh snapshot. The read cursor stays on one
        # snapshot while the final check detects replacement during the copy.
        db.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
        header = db.execute(query, identity).fetchone()
        if header is None:
            raise ValueError("this build is not the eligible service's ready candidate")
        digest, size, manifest, config = header
        if not 0 < size <= MAX_IMAGE_BYTES:
            raise ValueError("invalid candidate size")
        copied = 0
        checksum = hashlib.sha256()
        with private_archive(path) as output:
            with db.cursor(name="patch_image_chunks") as chunks:
                chunks.itersize = 1
                chunks.execute('''SELECT part,payload FROM "AdPatchCandidateChunks"
                    WHERE service_id=%s ORDER BY part''', (service,))
                for index, (part, payload) in enumerate(chunks):
                    expected = min(1024 * 1024, size - copied)
                    if part != index or len(payload) != expected or expected <= 0:
                        raise ValueError("invalid candidate chunk sequence")
                    output.write(payload)
                    checksum.update(payload)
                    copied += len(payload)
            if copied != size or checksum.hexdigest() != digest:
                raise ValueError("candidate archive size or digest mismatch")
            # Keep a replacement writer from retiring this candidate between
            # the identity check and publication of the verified local file.
            if db.execute(query + " FOR SHARE OF c", identity).fetchone() != header:
                raise ValueError("candidate changed during export")
    return ImageReceipt(digest, manifest, config)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service", required=True, type=int)
    parser.add_argument("--revision", required=True, type=int)
    parser.add_argument("--attempt", required=True, type=uuid.UUID)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--json", action="store_true", help="include the registry manifest and Docker config digests")
    args = parser.parse_args()
    try:
        with psycopg.connect(os.environ["RSCTF_DATABASE_URL"], autocommit=True) as db:
            receipt = export_candidate(db, args.service, args.revision, args.attempt, args.output)
    except psycopg.Error as error:
        sys.exit(f"could not export private build (PostgreSQL {error.sqlstate or 'connection error'})")
    except (OSError, ValueError, KeyError) as error:
        sys.exit(str(error))
    print(receipt.format(args.json))


if __name__ == "__main__":
    main()
