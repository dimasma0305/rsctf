FROM moby/buildkit:v0.32.2@sha256:28a898719c18a33f4e8000685287fa36fd0dd9560c6440227d3a732d79bb41d8 AS buildkit
FROM alpine:3.21@sha256:bc21487101ae47172509130ee1092cc273a831968d64b617776d1f69c81e42ab AS pack
RUN --mount=type=secret,id=apk_ca,target=/etc/ssl/certs/ca-certificates.crt apk add --no-cache cpio linux-virt
COPY --from=buildkit /usr/bin/buildkitd /usr/bin/buildctl /usr/bin/buildkit-runc /buildkit/
COPY init make-initramfs.sh /builder/
RUN sh /builder/make-initramfs.sh /buildkit /guest
FROM scratch
COPY --from=pack /guest/ /
