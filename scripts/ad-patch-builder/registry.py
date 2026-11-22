"""Repository naming and credential isolation shared by patch registry commands."""

import os
import re
import subprocess
import tempfile
from contextlib import contextmanager

from build import DEADLINE_SECONDS


@contextmanager
def registry_writer(db, service):
    # All mutations to a team repository share this session lock. Scoring
    # and builds can proceed while skopeo is transferring blobs.
    if not db.execute("SELECT pg_try_advisory_lock(1734271013,%s)", (service,)).fetchone()[0]:
        raise ValueError("another registry transfer is running for this service")
    try:
        yield
    finally:
        db.execute("SELECT pg_advisory_unlock(1734271013,%s)", (service,))


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
