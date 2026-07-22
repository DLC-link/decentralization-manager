# Runtime-only image. The binary is compiled in CI by the reusable
# build-binary.yml workflow (used by both build.yml for per-commit private dev
# images and release.yml for tagged public images) and passed in via the build
# context, so this Dockerfile does no compilation at all — it just wraps the
# prebuilt binary.
#
# The local dev / docker-compose full-source build lives in development/Dockerfile.
#
# distroless/cc-debian12 already ships glibc, libgcc, libssl/libcrypto and
# ca-certificates on Debian 12 (bookworm) — the same libs the binary is built
# against in the rust:slim-bookworm CI job — so nothing needs to be copied in.
FROM gcr.io/distroless/cc-debian12

COPY --chmod=0755 dec-party-manager /usr/local/bin/dec-party-manager

EXPOSE 8080 9000

# Image defaults; override via env at run time.
ENV DECPM_DIR=/ \
    DECPM_HOST=0.0.0.0 \
    DECPM_PORT=8080

ENTRYPOINT ["dec-party-manager", "serve"]
