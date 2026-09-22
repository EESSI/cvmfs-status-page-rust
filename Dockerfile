FROM rust:1-bookworm AS build
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends cmake perl && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY apps ./apps
COPY config.json ./
RUN cargo build --release --locked -p cvmfs-status-page-rust --bin cvmfs-status-server \
    && cp target/release/cvmfs-status-server /usr/local/bin/cvmfs-status-server && rm -rf target

FROM build AS musl-check
COPY scripts/check-static-binary.sh ./scripts/check-static-binary.sh
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools && rm -rf /var/lib/apt/lists/* \
    && target="$(uname -m)-unknown-linux-musl" && rustup target add "$target" \
    && CC=musl-gcc cargo test --workspace --all-targets --all-features --locked --target "$target" \
    && sh scripts/check-static-binary.sh "target/$target/debug/cvmfs-status-page-rust" \
    && sh scripts/check-static-binary.sh "target/$target/debug/cvmfs-status-server" && rm -rf target

FROM debian:bookworm-slim AS production
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /var/lib/cvmfs-status /etc/cvmfs-status && chown 10001:10001 /var/lib/cvmfs-status
COPY --from=build /usr/local/bin/cvmfs-status-server /usr/local/bin/cvmfs-status-server
USER 10001:10001
WORKDIR /var/lib/cvmfs-status
EXPOSE 8080
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/cvmfs-status-server"]
CMD ["--configuration", "/etc/cvmfs-status/config.json", "--state-directory", "/var/lib/cvmfs-status", "--public-address", "0.0.0.0:8080"]
