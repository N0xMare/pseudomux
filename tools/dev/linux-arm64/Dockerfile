# syntax=docker/dockerfile:1
#
# The linux/aarch64 measurement lane. Build context is the pseudomux checkout.
#
# This image exists so `linux`/`aarch64` can be MEASURED. It is not a dev-loop
# mirror and it is not an emulator: on an Apple Silicon host with a native
# linux/arm64 daemon nothing here is translated, so `std::env::consts::ARCH`
# in the pmuxd built below reports `aarch64` truthfully and the receipts are
# filed under the platform they were taken on. Read
# `docs/engineering/tart-linux-guest.md` sec.2 for the case this is NOT: an
# x86_64 build under Rosetta reports `linux`/`x86_64`, matches the shipped
# promoted cell, and applies a drain measured on real x86 silicon to an
# emulated one. That remains refused.
#
# rust-toolchain.toml pins 1.88.0; the FROM matches it rather than floating.
# The platform comes from `--platform linux/arm64` on the build, not a FROM pin.

FROM rust:1.88.0-bookworm AS build
WORKDIR /src
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        pkg-config libssl-dev ca-certificates build-essential \
    && rm -rf /var/lib/apt/lists/*
COPY . .
# pmux-mcp is ask/Messages over stdio and no tool in tools/dev launches it.
RUN cargo build --locked --release \
        -p pmux -p pmuxd -p pmux-rmuxd -p pmux-launcher -p pmux-hook


# The official Claude Code linux-arm64 (glibc) builds. `package/claude` is a
# native ELF, so no node runtime is installed anywhere in this image.
#
# TWO versions, because one is not enough to promote. The pooled bound in
# `evidence/pooled-transcript-drain-<os>-<arch>.json` must name at least two
# Claude versions, each contributing a reachable arrival of its own --
# `compatibility.rs::every_promoted_drain_is_the_pooled_bound_and_not_a_per_version_fit`
# refuses a one-version pool as "a per-version fit wearing a different noun".
# 2.1.258 is the version macos pools with 2.1.272, so linux/aarch64 pools the
# same pair.
#
# Both digests are pinned. The sha512 is the npm registry's own
# `dist.integrity` for the platform package, re-checked against the bytes; the
# sha256 is of the extracted executable and is the value the lane's README and
# the promotion report quote.
FROM debian:bookworm-slim AS claude
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        ca-certificates curl openssl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /fetch
COPY tools/dev/linux-arm64/install-claude.sh /fetch/install-claude.sh
RUN /fetch/install-claude.sh 2.1.258 \
        sha512-cbzsJZ4gMDp30eOlhA6qM+s0plv0TTKLhWX/y4CwNRspSQI8wPO5T1UafhimU4fTeznpPxH38m3k3vYkA9ypHw== \
        43dc490af55262edcb3e9b1cb315de22cc09ccb08bd52a4c39bc5eabaa63100f
RUN /fetch/install-claude.sh 2.1.272 \
        sha512-tpUGL+bDziPYaBplXrR018NdSE191nNvQGDHv9QluLB7u4h3t4iRSVnZ8e2KVJerAkER7JeS76sdRGoMPRSIfA== \
        214a90efdd16ee0ea81132ffecced588dba394d178cc494f285ba04b5288c8de


FROM debian:bookworm-slim AS runtime
# python3 -- every tool in tools/dev and tools/promotion is stdlib-only Python.
# procps -- promote_claude_version.py observes the pool with `pgrep -f`.
# sudo -- the last step of a run hands /src/evidence back to the host uid.
# No tmux: the transparent terminal profile prunes the tmux shim from PATH and
# no tool in this lane shells out to it.
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        python3 procps sudo ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Claude Code refuses --dangerously-skip-permissions as root, and a Full cell
# in living_pmux_run.py needs it, so the whole lane runs unprivileged.
#
# uid/gid 501 is the Darwin host user. Docker Desktop's virtiofs passes bind
# mounts through numerically, so a receipt this container writes into
# /src/evidence already belongs to the host user; `run.sh` still chowns at the
# end for the group and for anything created as root.
ARG HOST_UID=501
ARG HOST_GID=501
RUN groupadd -g "${HOST_GID}" pmux \
    && useradd -m -u "${HOST_UID}" -g "${HOST_GID}" -s /bin/bash pmux \
    && echo 'pmux ALL=(ALL) NOPASSWD: /bin/chown' > /etc/sudoers.d/pmux-chown \
    && chmod 0440 /etc/sudoers.d/pmux-chown

COPY --from=build /src/target/release/pmux          /opt/pmux/release/pmux
COPY --from=build /src/target/release/pmuxd         /opt/pmux/release/pmuxd
COPY --from=build /src/target/release/pmux-rmuxd    /opt/pmux/release/pmux-rmuxd
COPY --from=build /src/target/release/pmux-launcher /opt/pmux/release/pmux-launcher
COPY --from=build /src/target/release/pmux-hook     /opt/pmux/release/pmux-hook

# Where every tool in tools/dev expects it: --claude
# "$HOME/.local/share/pmux/claude/<version>/claude".
COPY --from=claude --chown=${HOST_UID}:${HOST_GID} \
        /out/ /home/pmux/.local/share/pmux/claude/

# The corpus a pooled bound is measured over. A campaign's transcripts live in
# the daemon's Path B evidence mirror, which is a sibling of the socket inside
# the run's sandbox; `--keep-sandbox` leaves them here so a later
# measure_transcript_drain.py run can pool two versions over them. Nothing
# here is committed: these are real prompts.
RUN install -d -m 0700 -o "${HOST_UID}" -g "${HOST_GID}" /corpus

COPY --chown=${HOST_UID}:${HOST_GID} tools/dev/linux-arm64/entrypoint.sh /usr/local/bin/lane
RUN chmod 0755 /usr/local/bin/lane

USER pmux
WORKDIR /src
ENV HOME=/home/pmux \
    PMUX_LANE_RELEASE_DIR=/opt/pmux/release \
    PMUX_LANE_CORPUS=/corpus
ENTRYPOINT ["/usr/local/bin/lane"]
CMD ["shell"]
