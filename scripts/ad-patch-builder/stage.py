"""Stage one ready, private team build in an organizer-controlled OCI registry."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import uuid

import psycopg

from build import DEADLINE_SECONDS
from export import export_candidate


def service_repository(prefix, service):
    host, sep, namespace = prefix.partition("/")
    label = r"[a-z0-9](?:[a-z0-9-]*[a-z0-9])?"
    address = re.fullmatch(rf"({label}(?:\.{label})*)(?::([0-9]{{1,5}}))?", host)
    # Short names without a port can silently select Docker Hub.
    if (not address or not sep or (host != "localhost" and "." not in host and ":" not in host)
            or (address[2] and not 1 <= int(address[2]) <= 65535)
            or not re.fullmatch(r"[a-z0-9]+(?:(?:[._/]|__|-+)[a-z0-9]+)*", namespace)
            or not 0 < service <= 2147483647):
        raise ValueError("expected an explicit registry host and lowercase repository namespace")
    repository = f"{prefix}/service-{service}"
    if len(repository) > 255:
        raise ValueError("registry repository name is too long")
    return repository


def skopeo(arguments, **options):
    # The registry client has no use for the worker's PostgreSQL credentials.
    allowed = ("PATH", "HOME", "LD_LIBRARY_PATH", "XDG_CONFIG_HOME", "XDG_RUNTIME_DIR", "TMPDIR",
               "REGISTRY_AUTH_FILE", "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY", "http_proxy",
               "https_proxy", "no_proxy", "SSL_CERT_FILE", "SSL_CERT_DIR")
    env = {key: os.environ[key] for key in allowed if key in os.environ}
    subprocess.run(["skopeo", "--tmpdir", tempfile.gettempdir(), *arguments], env=env, check=True, timeout=DEADLINE_SECONDS,
                   stderr=subprocess.DEVNULL, **options)


def verify_manifest(source, receipt, options):
    # Bound Python memory independently of the registry response size.
    with tempfile.TemporaryFile() as data:
        skopeo(["inspect", "--raw", *options, source], stdout=data)
        data.seek(0)
        raw = data.read(1024 * 1024 + 1)
    if len(raw) > 1024 * 1024 or "sha256:" + hashlib.sha256(raw).hexdigest() != receipt.manifest_digest:
        raise ValueError("staged manifest differs from the build receipt")
    manifest = json.loads(raw)
    if (not isinstance(manifest, dict) or manifest.get("schemaVersion") != 2
            or not isinstance(manifest.get("config"), dict)
            or manifest["config"].get("urls")
            or manifest["config"].get("digest") != receipt.config_digest):
        raise ValueError("staged image configuration differs from the build receipt")
    layers = manifest.get("layers")
    if not isinstance(layers, list) or any(not isinstance(layer, dict) or layer.get("urls") for layer in layers):
        raise ValueError("candidate has invalid or externally referenced layers")


def push_candidate(image, repository, receipt, authfile, cert_dir):
    remote = f"docker://{repository}@{receipt.manifest_digest}"
    credentials = ["--authfile", str(authfile)] if authfile else []
    trust = ["--cert-dir", str(cert_dir)] if cert_dir else []
    dest_credentials = ["--dest-authfile", str(authfile)] if authfile else []
    dest_trust = ["--dest-cert-dir", str(cert_dir)] if cert_dir else []
    archive = f"oci-archive:{image}"
    verify_manifest(archive, receipt, [])
    skopeo(["copy", "--preserve-digests", "--dest-tls-verify=true",
            *dest_credentials, *dest_trust, archive, remote], stdout=subprocess.DEVNULL)
    verify_manifest(remote, receipt, ["--tls-verify=true", *credentials, *trust])


def stage_candidate(db, service, revision, attempt, prefix, authfile=None, cert_dir=None):
    repository = service_repository(prefix, service)
    if not db.autocommit:
        raise ValueError("staging requires an autocommit database connection")
    # Serialize registry writers for this service without blocking scoring or builds.
    if not db.execute("SELECT pg_try_advisory_lock(1734271013,%s)", (service,)).fetchone()[0]:
        raise ValueError("another registry transfer is running for this service")
    try:
        return stage_locked(db, service, revision, attempt, repository, authfile, cert_dir)
    finally:
        db.execute("SELECT pg_advisory_unlock(1734271013,%s)", (service,))


def stage_locked(db, service, revision, attempt, repository, authfile, cert_dir):
    with tempfile.TemporaryDirectory(prefix="rsctf-stage-") as work:
        image = Path(work) / "candidate.oci.tar"
        receipt = export_candidate(db, service, revision, attempt, image)
        if not receipt.manifest_digest or not receipt.config_digest:
            raise ValueError("rebuild with a guest that records OCI image identities before staging")
        ref = f"{repository}@{receipt.manifest_digest}"
        existing = db.execute('''SELECT registry_image_ref FROM "AdPatchCandidates"
            WHERE service_id=%s AND revision=%s AND digest=%s''',
            (service, revision, receipt.archive_hash)).fetchone()
        if existing is None or existing[0] not in (None, ref):
            raise ValueError("candidate was replaced or already staged to a different repository")
        push_candidate(image, repository, receipt, authfile, cert_dir)
    # No transaction or row lock spans registry I/O. A superseding build,
    # withdrawn permission or different successful staging invalidates this write.
    recorded = db.execute('''UPDATE "AdPatchCandidates" c SET registry_image_ref=%s
        FROM "AdPatchBuilds" b JOIN "AdPatchBuildEligibleServices" e USING(service_id)
        WHERE c.service_id=%s AND c.revision=%s AND b.attempt=%s AND b.status='Ready'
          AND b.service_id=c.service_id AND b.revision=c.revision
          AND c.digest=%s AND c.manifest_digest=%s AND c.config_digest=%s
          AND (c.registry_image_ref IS NULL OR c.registry_image_ref=%s)''',
        (ref, service, revision, attempt, receipt.archive_hash, receipt.manifest_digest, receipt.config_digest, ref))
    if recorded.rowcount != 1:
        raise ValueError("the build, its eligibility or its staged repository changed during transfer")
    return ref


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--service", required=True, type=int)
    parser.add_argument("--revision", required=True, type=int)
    parser.add_argument("--attempt", required=True, type=uuid.UUID)
    parser.add_argument("--repository", required=True, help="private registry host and repository namespace")
    parser.add_argument("--authfile", type=Path, help="containers authentication file for the private registry")
    parser.add_argument("--cert-dir", type=Path, help="registry CA and optional client certificate directory")
    args = parser.parse_args()
    try:
        with psycopg.connect(os.environ["RSCTF_DATABASE_URL"], autocommit=True) as db:
            ref = stage_candidate(db, args.service, args.revision, args.attempt,
                                  args.repository, args.authfile, args.cert_dir)
    except psycopg.Error as error:
        sys.exit(f"could not stage private build (PostgreSQL {error.sqlstate or 'connection error'})")
    except subprocess.SubprocessError:
        sys.exit("registry staging failed: check skopeo, its image policy, registry access and TLS trust")
    except (OSError, ValueError, KeyError) as error:
        sys.exit(str(error))
    print(ref)


if __name__ == "__main__":
    main()
