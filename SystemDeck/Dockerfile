# SystemDeck server image (Linux). Build context: this workspace folder.
#   docker build -t systemdeck-server .
# Data (database + server.key) lives in /data - mount a volume there.

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
# Cache the crate registry and build output between rebuilds.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p systemdeck-server \
    && cp target/release/systemdeck-server /usr/local/bin/systemdeck-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 1000 --user-group --no-create-home --shell /usr/sbin/nologin systemdeck \
    && mkdir -p /data /downloads && chown systemdeck:systemdeck /data
COPY --from=build /usr/local/bin/systemdeck-server /usr/local/bin/systemdeck-server
ENV SYSTEMDECK_BIND=0.0.0.0:8740 \
    SYSTEMDECK_DATA=/data \
    SYSTEMDECK_DOWNLOADS=/downloads
USER systemdeck
EXPOSE 8740
VOLUME ["/data"]
# The server shuts down cleanly on SIGINT (Ctrl+C handler).
STOPSIGNAL SIGINT
ENTRYPOINT ["systemdeck-server"]
