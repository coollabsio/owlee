# syntax=docker/dockerfile:1.7
# UI assets are architecture-independent. Build them on BUILDPLATFORM so
# a cross-build does not emulate Bun. CI builds amd64 and arm64 on native
# runners, where this stage is native too.
FROM --platform=$BUILDPLATFORM oven/bun:1.4.5 AS web
WORKDIR /src/apps/web
COPY apps/web/package.json apps/web/bun.lock ./
RUN bun install --frozen-lockfile
COPY apps/web/ ./
RUN bun run build

# Matches rust-toolchain.toml, so rustup does not download a second toolchain.
FROM rust:1.99.1-bookworm AS server
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY apps/server/ apps/server/
COPY --from=web /src/apps/web/dist/ apps/web/dist/
RUN cargo build --locked --release -p owlee-server

FROM debian:bookworm-20261104-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 65532 --user-group --create-home --home-dir /var/lib/owlee owlee \
    && mkdir -p /var/lib/owlee \
    && chown -R 65532:65532 /var/lib/owlee
COPY --from=server /src/target/release/owlee /owlee
ENV OWLEE_ENV=production \
    OWLEE_BIND=0.0.0.0 \
    OWLEE_PORT=8080 \
    OWLEE_DATABASE_PATH=/var/lib/owlee/owlee.sqlite
USER 65532:65532
VOLUME ["/var/lib/owlee"]
EXPOSE 8080
# /api/health/ready also queries the database. The 2s start interval lets
# Coolify see the first healthy result without waiting a full 30s.
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --start-interval=2s --retries=3 CMD ["/owlee", "healthcheck"]
ENTRYPOINT ["/owlee"]
CMD ["serve"]
