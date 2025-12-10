FROM rust:slim-bookworm AS builder

RUN apt-get update
RUN apt-get install -y cmake pkg-config libssl-dev git openssh-client protobuf-compiler -y

WORKDIR /app

RUN mkdir -p /root/.ssh && ssh-keyscan github.com >> /root/.ssh/known_hosts

ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN --mount=type=ssh cargo build --release

FROM busybox:latest AS runtime

WORKDIR /app

COPY --from=builder /lib/x86_64-linux-gnu/libgcc_s.so.1 /lib64/libgcc_s.so.1
COPY --from=builder /lib/x86_64-linux-gnu/libssl.so.3 /lib64/libssl.so.3
COPY --from=builder /lib/x86_64-linux-gnu/libcrypto.so.3 /lib64/libcrypto.so.3
COPY --from=builder /app/target/release/dec-party-onboarding /usr/local/bin/

EXPOSE 8080

ENTRYPOINT ["dec-party-onboarding", "-c", "/config/node.toml", "serve", "--host", "0.0.0.0", "--port", "8080"]
