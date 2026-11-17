# syntax=docker/dockerfile:1
#
# The image the demo GIFs are recorded in. `iktinos` is built here, from
# the sources alone (see Dockerfile.dockerignore), and VHS runs here with
# only `tape/` and `assets/` mounted: nothing of the recording machine — no
# home directory, no `~/.claude` — exists inside. See tape/record.sh.

FROM rust:1-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p iktinos \
    && cp target/release/iktinos /iktinos

FROM ghcr.io/charmbracelet/vhs:v0.12.1
COPY --from=build /iktinos /usr/local/bin/iktinos
# What iktinos would read by default is under HOME: an empty directory.
ENV HOME=/demo/home
RUN mkdir -p /demo/home /demo/assets
WORKDIR /demo
