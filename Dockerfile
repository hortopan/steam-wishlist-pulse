FROM alpine:3.21 AS downloader
ARG TARGETARCH
ARG VERSION
RUN apk add --no-cache curl tar xz
RUN case "${TARGETARCH}" in \
      amd64) ARCH="x86_64" ;; \
      arm64) ARCH="aarch64" ;; \
      *) echo "Unsupported architecture: ${TARGETARCH}" && exit 1 ;; \
    esac && \
    curl -fsSL "https://github.com/hortopan/steam-wishlist-pulse/releases/download/v${VERSION}/wishlist-pulse-${ARCH}-unknown-linux-musl.tar.xz" \
      | tar -xJ --strip-components=1 -C /usr/local/bin

FROM scratch
COPY --from=downloader /usr/local/bin/wishlist-pulse /wishlist-pulse
COPY --from=downloader /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
EXPOSE 3000
VOLUME ["/data"]
ENV DATABASE_PATH=/data/wishlist-pulse.db
ENTRYPOINT ["/wishlist-pulse"]
