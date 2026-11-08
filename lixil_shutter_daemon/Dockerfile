ARG BUILD_FROM

# --- build stage: glibc (Debian), matches the runtime base below ---
FROM rust:1-slim-bookworm AS build

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libdbus-1-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY shutter-daemon/Cargo.toml shutter-daemon/Cargo.lock* ./
COPY shutter-daemon/src ./src
COPY shutter-daemon/assets ./assets
RUN cargo build --release && strip target/release/shutter-daemon

# --- runtime stage: HA community base image (Debian/glibc, s6-overlay + bashio preinstalled) ---
FROM ${BUILD_FROM}

RUN apt-get update && apt-get install -y --no-install-recommends \
    libdbus-1-3 \
    bluez \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /build/target/release/shutter-daemon /usr/bin/shutter-daemon
COPY rootfs /
