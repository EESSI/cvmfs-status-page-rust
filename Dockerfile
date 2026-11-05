ARG BINARY_SOURCE=build

FROM rust:1-bookworm AS build
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends cmake perl musl-tools && rm -rf /var/lib/apt/lists/* \
    && rustup target add "$(uname -m)-unknown-linux-musl"
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY apps ./apps
COPY config.json ./
COPY scripts/check-static-binary.sh ./scripts/check-static-binary.sh
RUN target="$(uname -m)-unknown-linux-musl" \
    && CC=musl-gcc cargo build --release --locked --target "$target" -p cvmfs-status-page-rust --bin cvmfs-status-server \
    && sh scripts/check-static-binary.sh "target/$target/release/cvmfs-status-server" \
    && cp "target/$target/release/cvmfs-status-server" /usr/local/bin/cvmfs-status-server && rm -rf target

FROM build AS musl-check
RUN target="$(uname -m)-unknown-linux-musl" \
    && CC=musl-gcc cargo test --workspace --all-targets --all-features --locked --target "$target" \
    && sh scripts/check-static-binary.sh "target/$target/debug/cvmfs-status-page-rust" \
    && sh scripts/check-static-binary.sh "target/$target/debug/cvmfs-status-server" && rm -rf target

# Release builds select this stage to reuse the verified musl archive binary.
FROM scratch AS prebuilt
COPY dist/cvmfs-status-server /usr/local/bin/cvmfs-status-server

FROM ${BINARY_SOURCE} AS binary

FROM debian:bookworm-slim AS runtime-files
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /rootfs/etc/ssl/certs /rootfs/etc/cvmfs-status /rootfs/var/lib/cvmfs-status /rootfs/tmp \
    && cp /etc/ssl/certs/ca-certificates.crt /rootfs/etc/ssl/certs/ \
    && chown 10001:10001 /rootfs/var/lib/cvmfs-status && chmod 1777 /rootfs/tmp

FROM scratch AS production
LABEL org.opencontainers.image.source="https://github.com/EESSI/cvmfs-status-page-rust"
COPY --from=runtime-files /rootfs/ /
COPY --from=binary --chmod=755 /usr/local/bin/cvmfs-status-server /usr/local/bin/cvmfs-status-server
ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
USER 10001:10001
WORKDIR /var/lib/cvmfs-status
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/cvmfs-status-server"]
CMD ["--configuration", "/etc/cvmfs-status/config.json", "--state-directory", "/var/lib/cvmfs-status", "--public-address", "0.0.0.0:8080"]
