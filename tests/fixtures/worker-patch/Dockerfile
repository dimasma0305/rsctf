FROM rsctf-worker-smoke:base
USER root
RUN mkdir -p /data && chown nobody:nogroup /data
COPY src/revision /usr/local/share/rsctf-fixture-revision
USER nobody
