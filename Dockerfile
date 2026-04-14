FROM rust:slim-bookworm AS builder

RUN apt-get update
RUN apt-get install -y cmake pkg-config libssl-dev git openssh-client protobuf-compiler nodejs npm curl

WORKDIR /app

RUN mkdir -p /root/.ssh && ssh-keyscan github.com >> /root/.ssh/known_hosts

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

COPY frontend/package.json frontend/package-lock.json ./frontend/
RUN cd frontend && npm ci

COPY Cargo.toml Cargo.lock build.rs ./
COPY migrations ./migrations
COPY src ./src
COPY frontend/src ./frontend/src
COPY frontend/index.html frontend/vite.config.ts frontend/tsconfig*.json frontend/eslint.config.js frontend/.env ./frontend/

RUN --mount=type=ssh cargo build --release

FROM busybox:latest AS runtime

WORKDIR /app

COPY --from=builder /lib/x86_64-linux-gnu/libgcc_s.so.1 /lib64/libgcc_s.so.1
COPY --from=builder /lib/x86_64-linux-gnu/libssl.so.3 /lib64/libssl.so.3
COPY --from=builder /lib/x86_64-linux-gnu/libcrypto.so.3 /lib64/libcrypto.so.3
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /app/target/release/dec-party-manager /usr/local/bin/

EXPOSE 8080 9000

ENTRYPOINT ["dec-party-manager", "-d", "/", "serve", "--host", "0.0.0.0", "--port", "8080"]
