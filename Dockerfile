FROM rust:slim-bookworm AS builder

RUN apt-get update && apt-get install -y curl ca-certificates
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
RUN apt-get install -y cmake pkg-config libssl-dev git openssh-client protobuf-compiler nodejs

WORKDIR /app

RUN mkdir -p /root/.ssh && ssh-keyscan github.com >> /root/.ssh/known_hosts

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

# Frontend deps layer, cached unless package*.json change. The frontend now
# lives under the backend crate (crates/decman/frontend).
COPY crates/decman/frontend/package.json crates/decman/frontend/package-lock.json ./crates/decman/frontend/
RUN cd crates/decman/frontend && npm ci

# Workspace manifest + the sibling crates the backend depends on / the
# workspace must resolve. `common` and `decman-cli` are copied wholesale (small,
# no node_modules); the backend crate is copied selectively so the host's
# frontend/node_modules is never pulled in.
COPY Cargo.toml Cargo.lock ./
COPY crates/common ./crates/common
COPY crates/decman-cli ./crates/decman-cli
COPY crates/decman/Cargo.toml crates/decman/build.rs ./crates/decman/
COPY crates/decman/migrations ./crates/decman/migrations
COPY crates/decman/src ./crates/decman/src
COPY crates/decman/frontend/src ./crates/decman/frontend/src
COPY crates/decman/frontend/public ./crates/decman/frontend/public
COPY crates/decman/frontend/index.html crates/decman/frontend/vite.config.ts crates/decman/frontend/tsconfig*.json crates/decman/frontend/eslint.config.js crates/decman/frontend/.env ./crates/decman/frontend/

# Generate the frontend's TypeScript wire types from the Rust DTOs (ts-rs),
# which `build.rs` needs before it builds the frontend. DECMAN_SKIP_FRONTEND so
# this generation build doesn't itself try to build the frontend (chicken-and-egg).
# Same --release profile so the dependency compiles are shared with the build below.
RUN --mount=type=ssh DECMAN_SKIP_FRONTEND=1 \
    cargo run --release -p decman --features typegen --bin gen-types

# Build only the backend (its bin is `dec-party-manager`). `-p decman` avoids
# compiling the `decman-cli` TUI, whose Linux file-dialog backend would pull in
# extra system libraries the server image doesn't need.
RUN --mount=type=ssh cargo build --release -p decman

FROM busybox:latest AS runtime

WORKDIR /app

COPY --from=builder /lib/x86_64-linux-gnu/libgcc_s.so.1 /lib64/libgcc_s.so.1
COPY --from=builder /lib/x86_64-linux-gnu/libssl.so.3 /lib64/libssl.so.3
COPY --from=builder /lib/x86_64-linux-gnu/libcrypto.so.3 /lib64/libcrypto.so.3
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /app/target/release/dec-party-manager /usr/local/bin/

EXPOSE 8080 9000

# Image defaults; override via env at run time. Every other knob is already
# DECPM_*, keep the CLI invocation flag-free for consistency.
ENV DECPM_DIR=/ \
    DECPM_HOST=0.0.0.0 \
    DECPM_PORT=8080

ENTRYPOINT ["dec-party-manager", "serve"]
