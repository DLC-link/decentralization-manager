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
#
# The `:nonroot` tag is the same image with uid/gid 65532 and a home directory
# it owns. The binary needs no root: it binds 8080 and 9000, both above 1024,
# and writes only under its data directory.
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --chmod=0755 dec-party-manager /usr/local/bin/dec-party-manager

# Stated explicitly rather than inherited, so the runtime identity is visible
# here and survives a base-image change.
USER 65532:65532

EXPOSE 8080 9000

# Build identity, stamped by CI (build.yml / release.yml). DECPM_BUILD_VERSION
# is the pushed image tag (releases) or short commit SHA (per-commit images);
# it's what the UI shows as the build version. Left blank on a plain
# `docker build`, in which case the app falls back to `<cargo-semver>-dev`.
ARG DECPM_BUILD_VERSION=
ARG DECPM_BUILD_TIME=

# Image defaults; override via env at run time.
#
# DECPM_DIR is the ROOT dir; the app writes to `$DECPM_DIR/data`. It defaults to
# the nonroot home directory because uid 65532 cannot write to `/`, so the old
# `/` default would put the database at an unwritable `/data`. Deployments
# override it anyway (the guide mounts a volume and passes `-d /app`), but a
# plain `docker run` now works instead of failing on the first write.
ENV DECPM_DIR=/home/nonroot \
    DECPM_HOST=0.0.0.0 \
    DECPM_PORT=8080 \
    DECPM_BUILD_VERSION=$DECPM_BUILD_VERSION \
    DECPM_BUILD_TIME=$DECPM_BUILD_TIME

ENTRYPOINT ["dec-party-manager", "serve"]
