# Linux reference platform (PLAN.md D11): Rocky Linux 9, glibc 2.34, VFX Reference Platform CY2027.
# The same pinned base as OpenColorIO-rs. It builds upstream OpenEXR from the submodule with the
# system GCC (scripts/upstream-build.sh), runs the oracle wheel (manylinux_2_28 x86-64), and builds
# and tests the port with rust-toolchain.toml's toolchain (PLAN.md O5, in use since 2026-11-07).
FROM rockylinux/rockylinux:9.8@sha256:8101994123cf3d0a8fee517bee7f39e555c7d92bd2d9eb3303cc988a0eeed00f

# Fixed minor-release URLs also try Rocky's vault when 9.8 leaves the live tree.
COPY reference.repo /etc/openexr-rs-reference.repos.d/reference.repo
RUN dnf --releasever=9.8 --setopt=reposdir=/etc/openexr-rs-reference.repos.d \
        install -y --setopt=install_weak_deps=False \
        gcc gcc-c++ make git which findutils diffutils tar xz procps-ng \
    && dnf clean all

ENV UV_INSTALL_DIR=/opt/uv/bin \
    UV_PYTHON_INSTALL_DIR=/opt/uv/python \
    UV_CACHE_DIR=/opt/uv/cache \
    RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/opt/cargo \
    PATH=/opt/cargo/bin:/opt/uv/bin:$PATH

RUN curl -LsSf https://astral.sh/uv/0.11.7/install.sh | sh \
    && uv python install 3.13 \
    && chmod -R a+rwX /opt/uv

# The same toolchain as rust-toolchain.toml.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal --default-toolchain 1.99.1 -c rustfmt,clippy \
    && chmod -R a+w /opt/cargo /opt/rustup

# The checkout is mounted here; Linux build output goes to a volume at /work/target/rocky9,
# cargo's under it.
ENV CARGO_TARGET_DIR=/work/target/rocky9/cargo
WORKDIR /work
