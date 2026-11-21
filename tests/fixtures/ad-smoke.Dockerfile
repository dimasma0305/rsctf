# The HTTP fixture runs as nobody, the same identity used for round rotation.
ARG SMOKE_IMAGE
FROM ${SMOKE_IMAGE}
USER root
RUN touch /flag && chown nobody:nogroup /flag
USER nobody
