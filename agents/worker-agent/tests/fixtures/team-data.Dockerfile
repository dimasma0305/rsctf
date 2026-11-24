FROM rsctf-worker-smoke:base
USER root
RUN mkdir -p /data && chown nobody:nogroup /data
USER nobody
