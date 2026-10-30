# The relay as a container. Build it from the REPO ROOT (the server depends on
# the `protocol`, `host`, `sim` and `sdf` crates next to it, and Cargo needs the
# whole workspace to resolve Cargo.lock):
#
#     docker build -f server/Dockerfile -t craftiero-relay .
#     docker run -p 9001:8080 craftiero-relay   # then ws://127.0.0.1:9001/ws
#
# Two stages: compile with the whole Rust toolchain, then ship only the binary
# in a small image. Both stages name the same Debian release on purpose: a
# binary built against a newer glibc will not start on an older one.

FROM rust:1-slim-bookworm AS build
WORKDIR /app
COPY . .
# The workspace turns on fat LTO to shrink the browser's .wasm download. The
# server does not need that, and LTO can use more memory than a small free
# build machine has, so switch it off for this build only.
ENV CARGO_PROFILE_RELEASE_LTO=false
# --locked: build exactly the versions in Cargo.lock, never newer ones.
RUN cargo build -p server --release --locked

FROM debian:bookworm-slim
COPY --from=build /app/target/release/server /usr/local/bin/server
# The server listens on $PORT (hosting services set it themselves).
ENV PORT=8080
EXPOSE 8080
CMD ["server"]
