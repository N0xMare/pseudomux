# This file deliberately uses only the daemon-bundled Dockerfile frontend; it
# does not resolve a mutable `# syntax=` image. The runner records the exact
# Docker/Buildx versions and requires the base to be an externally reviewed
# multiarch manifest digest.
ARG PMUX_RUST_BASE
FROM ${PMUX_RUST_BASE}

ARG TARGETARCH
ARG PMUX_EXPECTED_SOURCE_SHA256
ARG PMUX_RUST_BASE

# The timestamped Debian repositories are immutable inputs. The exact signed
# InRelease payload hashes below were independently captured before this source
# was frozen; apt still performs its ordinary signature verification.
RUN rm -f /etc/apt/sources.list /etc/apt/sources.list.d/* \
    && printf '%s\n' \
      'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/20250725T000000Z bookworm main' \
      'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian/20250725T000000Z bookworm-updates main' \
      'deb [check-valid-until=no] https://snapshot.debian.org/archive/debian-security/20250725T000000Z bookworm-security main' \
      > /etc/apt/sources.list.d/pmux-snapshot.list \
    && apt-get -o Acquire::Retries=0 -o Acquire::Check-Valid-Until=false update \
    && set -- /var/lib/apt/lists/*_dists_bookworm_InRelease \
    && test "$#" -eq 1 \
    && echo '919b6d130d8afa68a8680a24db6a09a9ccdc9226188b42079cd3a3d6fad028de  '"$1" \
      | sha256sum --check --status \
    && set -- /var/lib/apt/lists/*_dists_bookworm-updates_InRelease \
    && test "$#" -eq 1 \
    && echo 'ee3934d9fb7836e3bf303fad2c0b02d366020367fa4e0f4092dae51f82dd0425  '"$1" \
      | sha256sum --check --status \
    && set -- /var/lib/apt/lists/*_dists_bookworm-security_InRelease \
    && test "$#" -eq 1 \
    && echo '2cbfcb4744de07ab4aebbe19466d6de02065ce47846d2a8274a26f6e06b3e4ea  '"$1" \
      | sha256sum --check --status \
    && DEBIAN_FRONTEND=noninteractive apt-get \
      -o Acquire::Retries=0 -o Acquire::Check-Valid-Until=false \
      install -y --no-install-recommends \
        ca-certificates \
        git \
        libdigest-sha-perl \
        nodejs \
        npm \
        procps \
        python3 \
        python3-venv \
        shellcheck \
        util-linux \
    && rm -rf /var/lib/apt/lists/* \
    && dpkg-query -W -f='${binary:Package}\t${Version}\n' \
      | LC_ALL=C sort > /opt/pmux-system-packages.tsv \
    && printf '%s\n' \
      'schema_version=1' \
      'snapshot=20250725T000000Z' \
      'debian_bookworm_inrelease_sha256=919b6d130d8afa68a8680a24db6a09a9ccdc9226188b42079cd3a3d6fad028de' \
      'debian_bookworm_updates_inrelease_sha256=ee3934d9fb7836e3bf303fad2c0b02d366020367fa4e0f4092dae51f82dd0425' \
      'debian_security_bookworm_inrelease_sha256=2cbfcb4744de07ab4aebbe19466d6de02065ce47846d2a8274a26f6e06b3e4ea' \
      > /opt/pmux-debian-snapshot.txt \
    && chmod 0444 \
      /opt/pmux-system-packages.tsv \
      /opt/pmux-debian-snapshot.txt

RUN groupadd --gid 10001 pmux \
    && useradd --uid 10001 --gid 10001 --create-home --shell /bin/bash pmux \
    && groupadd --gid 10002 intruder \
    && useradd --uid 10002 --gid 10002 --create-home --shell /bin/bash intruder \
    && install -d -o pmux -g pmux -m 0700 \
        /opt/pmux-candidate \
        /opt/pmux-candidate/build \
        /opt/pmux-candidate/bin \
        /opt/pmux-prefetch

ENV RUSTUP_TOOLCHAIN=1.88.0 \
    CARGO_TERM_COLOR=never \
    RUST_BACKTRACE=1 \
    TZ=UTC \
    PATH=/opt/pmux-python/bin:/opt/pmux-cargo-fuzz/bin:/usr/local/cargo/bin:/usr/local/bin:/usr/bin:/bin \
    PMUX_BASE_IMAGE_REF=${PMUX_RUST_BASE}

RUN rustup component add --toolchain 1.88.0 rustfmt clippy \
    && rustup toolchain install nightly-2026-03-26 --profile minimal \
    && rustup component add --toolchain nightly-2026-03-26 rustfmt clippy \
    && cargo install cargo-fuzz --version 0.13.2 --locked --root /opt/pmux-cargo-fuzz

WORKDIR /workspace
COPY . /workspace

# Prove that the allowlisted build context contains no host metadata,
# credentials, runtime artifacts, symlinks, or special nodes before any build
# tool creates its own excluded state.
RUN set -eu; \
    test -n "$PMUX_EXPECTED_SOURCE_SHA256"; \
    unexpected="$(find /workspace -mindepth 1 \( \
        -type l -o \
        -type d \( \
            -name .git -o -name .context -o -name .claude -o \
            -name target -o -name node_modules -o -name dist -o \
            -name __pycache__ -o -name .pytest_cache -o \
            -name .ruff_cache -o -name .mypy_cache \
        \) -o \
        -type f \( \
            -name .env -o -name '.env.*' -o \
            -name .npmrc -o -name .netrc -o \
            -name credentials -o -name credentials.toml -o \
            -name '*.pem' -o -name '*.key' -o -name '*.p12' -o -name '*.pfx' -o \
            -name '*.pid' -o -name '*.sock' -o -name '*.socket' -o \
            -name '*.fifo' -o -name 'core.[0-9]*' -o \
            -name '*.pyc' -o -name .coverage -o -name .DS_Store \
        \) -o -perm /022 -o \
        -type s -o -type p -o -type b -o -type c \
    \) -print -quit)"; \
    if [ -n "$unexpected" ]; then \
        echo "linux-docker: forbidden build-context artifact survived: $unexpected" >&2; \
        exit 1; \
    fi; \
    python3 tools/linux-docker/source_digest.py /workspace \
        --expected "$PMUX_EXPECTED_SOURCE_SHA256"

# Install only hash-locked wheels selected for the two supported glibc
# architectures. This source-owned requirements file is consumed only after
# the complete build context has passed its frozen-source check.
RUN python3 -m venv /opt/pmux-python \
    && /opt/pmux-python/bin/python -m pip install \
      --disable-pip-version-check \
      --no-cache-dir \
      --no-deps \
      --only-binary=:all: \
      --require-hashes \
      --requirement tools/linux-docker/python-requirements.txt

# The tracked fuzz gate records Git identity.  Build an image-local repository
# from the already-attested source; no host .git directory or remote enters the
# image.
RUN git init -q \
    && git config user.name pmux-linux-evidence \
    && git config user.email pmux-linux-evidence.invalid \
    && git add -A \
    && git commit -q -m frozen-source \
    && git config --system --add safe.directory /workspace \
    && npm --prefix clients/typescript ci --ignore-scripts --no-audit --no-fund

# Fetch and prebuild every offline runtime dependency while the image build is
# still networked. No build output enters the frozen source tree. The exact
# eight release executables are copied into their own candidate directory;
# later runtime gates may execute but cannot rebuild that directory.
USER pmux
RUN CARGO_TARGET_DIR=/opt/pmux-prefetch/workspace \
       cargo +1.88 test --locked --workspace --all-targets --all-features --no-run \
    && cargo +1.88 fetch --locked --manifest-path vendor/rmux-client/Cargo.toml \
    && CARGO_TARGET_DIR=/opt/pmux-prefetch/vendor-rmux-client \
       cargo +1.88 test --locked --manifest-path vendor/rmux-client/Cargo.toml \
       --all-targets --all-features --no-run \
    && cargo +1.88 fetch --locked --manifest-path vendor/rmux-server/Cargo.toml \
    && CARGO_TARGET_DIR=/opt/pmux-prefetch/vendor-rmux-server \
       cargo +1.88 test --locked --manifest-path vendor/rmux-server/Cargo.toml \
       --lib --no-default-features --no-run \
    && CARGO_TARGET_DIR=/opt/pmux-prefetch/vendor-rmux-server \
       cargo +1.88 test --locked --manifest-path vendor/rmux-server/Cargo.toml \
       --all-targets --all-features --no-run \
    && CARGO_TARGET_DIR=/opt/pmux-prefetch/workspace \
       cargo +1.88 test --locked -p pseudomux-claude \
       --test transcript_properties --no-run \
    && CARGO_TARGET_DIR=/opt/pmux-prefetch/workspace \
       cargo +1.88 test --locked -p pseudomux-service --test actor_model --no-run \
    && CARGO_TARGET_DIR=/opt/pmux-prefetch/workspace \
       cargo +1.88 test --locked -p pseudomux-client \
       --lib protocol_properties --no-run \
    && CARGO_TARGET_DIR=/opt/pmux-prefetch/fuzz \
       cargo +nightly-2026-03-26 fuzz build transcript_jsonl \
    && CARGO_TARGET_DIR=/opt/pmux-prefetch/fuzz \
       cargo +nightly-2026-03-26 fuzz build transcript_cursor \
    && CARGO_TARGET_DIR=/opt/pmux-prefetch/fuzz \
       cargo +nightly-2026-03-26 fuzz build native_frame \
    && CARGO_TARGET_DIR=/opt/pmux-candidate/build \
       cargo +1.88 build --locked --workspace --release --bins \
    && for binary in \
         pmux pmuxd pmux-rmuxd pmux-launcher pmux-hook pmux-mcp claude-p \
         pmux-test-claude; do \
         install -m 0500 "/opt/pmux-candidate/build/release/$binary" \
           "/opt/pmux-candidate/bin/$binary"; \
       done \
    && test "$(find /opt/pmux-candidate/bin -mindepth 1 -maxdepth 1 \
         -type f -links 1 | wc -l)" -eq 8 \
    && rm -rf /opt/pmux-candidate/build /opt/pmux-prefetch

USER root
RUN install -d -o pmux -g pmux -m 0700 /artifacts /var/tmp/pmux-linux-suite \
    && chown -R root:root /opt/pmux-candidate \
    && chmod 0555 /opt/pmux-candidate /opt/pmux-candidate/bin \
    && chmod 0555 /opt/pmux-candidate/bin/* \
    && chmod 0755 \
        tools/linux-docker/bounded_runner.py \
        tools/linux-docker/evidence.py \
        tools/linux-docker/inside.sh \
        tools/linux-docker/permissions_probe.py \
        tools/linux-docker/run.sh \
        tools/linux-docker/source_digest.py \
        tools/linux-docker/suite.sh

ENV CARGO_NET_OFFLINE=true \
    npm_config_audit=false \
    npm_config_fund=false \
    npm_config_offline=true \
    PMUX_FROZEN_SOURCE_SHA256=${PMUX_EXPECTED_SOURCE_SHA256} \
    PMUX_CONTAINER_PLATFORM=linux/${TARGETARCH}

LABEL org.opencontainers.image.title="pmux deterministic Linux test image" \
      org.opencontainers.image.description="Credential-free frozen-source Linux portability evidence" \
      io.pmux.source-sha256="${PMUX_EXPECTED_SOURCE_SHA256}" \
      io.pmux.target-arch="${TARGETARCH}" \
      io.pmux.base-image="${PMUX_RUST_BASE}" \
      io.pmux.apt-resolution="snapshot-20250725T000000Z" \
      io.pmux.python-requirements-sha256="e2438b6ee5a56701f219479b3bbd6b5c523ff779fa3de1c8d6fbadc4936d780a"

ENTRYPOINT ["/workspace/tools/linux-docker/inside.sh"]
