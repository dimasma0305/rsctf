"""Consume private patch revisions for one hosted A&D challenge."""
import argparse
from copy import copy
import hashlib
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

import psycopg
from psycopg.rows import dict_row

from build import build_to, DEADLINE_SECONDS


def register(db, args):
    profile = hashlib.sha256()
    for name in ("vmlinuz", "initramfs.cpio"):
        with (args.guest / name).open("rb") as source:
            profile.update(hashlib.file_digest(source, "sha256").digest())
    profile.update((args.base_manifest or "").encode())
    identity = profile.hexdigest()
    # A session lock both elects an owner and advertises its live connection.
    # Losing the connection abandons the claim. A successor assigns a new token.
    if not db.execute("SELECT pg_try_advisory_lock(1734271011,%s) AS owned",
                      (args.challenge,)).fetchone()["owned"]:
        raise ValueError("this challenge already has a connected builder")
    result = db.execute('''INSERT INTO "AdPatchBuilders"(challenge_id,profile,backend_pid)
        SELECT c.id,%s,pg_backend_pid() FROM "GameChallenges" c JOIN "AdPatchStarters" st ON st.challenge_id=c.id
        WHERE c.id=%s AND c."Type"=4 AND NOT c.ad_self_hosted AND NOT c.deletion_pending
        ON CONFLICT(challenge_id) DO UPDATE SET profile=excluded.profile,backend_pid=excluded.backend_pid''', (identity, args.challenge))
    if result.rowcount != 1:
        raise ValueError("challenge needs a hosted A&D service and a registered starter ZIP")
    return identity


def claim_next(db, challenge):
    return db.execute('''WITH next AS (
        SELECT b.attempt,r.tree_id FROM "AdPatchBuilds" b JOIN "AdTeamServices" s ON s.id=b.service_id
        JOIN "AdPatchRevisions" r ON r.service_id=b.service_id AND r.revision=b.revision
        WHERE s.challenge_id=%s AND b.status IN ('Building','Queued')
        ORDER BY b.queued_at,b.service_id LIMIT 1 FOR UPDATE OF b)
        UPDATE "AdPatchBuilds" b SET status='Building',claim=%s,claimed_by=pg_backend_pid() FROM next
        WHERE b.attempt=next.attempt RETURNING b.service_id,b.revision,b.attempt,b.claim,b.profile,next.tree_id''',
        (challenge, uuid.uuid4())).fetchone()


def publish(db, job, image, receipt):
    with db.transaction():
        current = db.execute('''SELECT b.service_id FROM "AdPatchBuilds" b
            JOIN "AdTeamServices" s ON s.id=b.service_id
            JOIN "GameChallenges" c ON c.id=s.challenge_id AND c.game_id=s.game_id
            JOIN "Games" g ON g.id=c.game_id
            WHERE b.attempt=%s AND b.claim=%s AND b.status='Building'
            AND c."Type"=4 AND NOT c.ad_self_hosted AND c.is_enabled AND NOT c.deletion_pending
            AND NOT g.deletion_pending AND g.start_time_utc<=clock_timestamp() AND clock_timestamp()<g.end_time_utc
            FOR SHARE OF g,c,s FOR UPDATE OF b''', (job["attempt"],job["claim"])).fetchone()
        if not current:
            raise ValueError("build claim or game changed")
        service = job["service_id"]
        size = image.tell()
        image.seek(0)
        db.execute('DELETE FROM "AdPatchCandidates" WHERE service_id=%s', (service,))
        db.execute('''INSERT INTO "AdPatchCandidates"
                      (service_id,revision,digest,archive_bytes,manifest_digest,config_digest)
                      VALUES(%s,%s,%s,%s,%s,%s)''',
                   (service, job["revision"], receipt.archive_hash, size,
                    receipt.manifest_digest, receipt.config_digest))
        part = 0
        while block := image.read(1024 * 1024):
            db.execute('INSERT INTO "AdPatchCandidateChunks"(service_id,part,payload) VALUES(%s,%s,%s)',
                       (service,part,block))
            part += 1
        db.execute('''UPDATE "AdPatchBuilds" SET status='Replaced' WHERE service_id=%s AND status='Ready' ''',
                   (service,))
        changed = db.execute('''UPDATE "AdPatchBuilds" b SET status='Ready',claim=NULL,claimed_by=NULL
                     WHERE attempt=%s AND claim=%s AND EXISTS (
                        SELECT 1 FROM "AdPatchBuildEligibleServices" e WHERE e.service_id=b.service_id)''',
                     (job["attempt"],job["claim"])).rowcount
        if changed != 1:
            raise ValueError("service is no longer eligible for patch builds")


def run_job(db, job, args, profile):
    try:
        if profile != job["profile"]:
            raise ValueError("builder profile changed after admission")
        with tempfile.TemporaryFile(dir=args.scratch) as source, tempfile.TemporaryFile(dir=args.scratch) as image:
            subprocess.run([args.rsctf, "challenge", "patch-job-source", str(job["attempt"]), str(job["claim"])],
                           stdout=source, check=True, timeout=DEADLINE_SECONDS)
            source.seek(0)
            build_args = copy(args)
            build_args.zip = "/proc/self/fd/0"
            build_args.tree = job["tree_id"]
            receipt = build_to(build_args, args.scratch, image, source=source)
            publish(db, job, image, receipt)
        return
    except psycopg.DatabaseError as error:
        if db.closed or db.broken:
            raise
        # PostgreSQL details may contain private source bytes. The rolled-back
        # publication keeps the previous candidate intact and the worker usable.
        reason = f"database error {error.sqlstate or 'unknown'}"
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        reason = str(error)
    print(f"Patch attempt {job['attempt']} failed: {reason}", file=sys.stderr, flush=True)
    db.execute('''UPDATE "AdPatchBuilds" SET status='Failed',claim=NULL,claimed_by=NULL
                  WHERE attempt=%s AND claim=%s AND status='Building' ''', (job["attempt"],job["claim"]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--challenge", type=int, required=True)
    parser.add_argument("--guest", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, default=None)
    parser.add_argument("--rsctf", default="rsctf")
    parser.add_argument("--base-oci", type=Path)
    parser.add_argument("--base-manifest")
    parser.add_argument("--kvm", action="store_true")
    parser.add_argument("--once", action="store_true", help="process at most one existing request")
    args = parser.parse_args()
    if os.getuid() == 0:
        parser.error("run the builder as an unprivileged account")
    if bool(args.base_oci) != bool(args.base_manifest):
        parser.error("provide both starter image options together")
    if args.base_oci and (not args.base_oci.is_file() or not re.fullmatch(r"sha256:[a-f0-9]{64}", args.base_manifest)):
        parser.error("expected a starter OCI file and its sha256 manifest digest")
    for tool in (args.rsctf, "qemu-system-x86_64"):
        if not shutil.which(tool):
            parser.error(f"worker executable unavailable: {tool}")
    with psycopg.connect(os.environ["RSCTF_DATABASE_URL"], autocommit=True, row_factory=dict_row) as db:
        profile = register(db, args)
        while True:
            job = claim_next(db, args.challenge)
            if job:
                run_job(db, job, args, profile)
            if args.once:
                return
            if not job:
                time.sleep(2)


if __name__ == "__main__":
    main()
