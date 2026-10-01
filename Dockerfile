# syntax=docker/dockerfile:1

# The headless shape is the only one that makes sense in a container: there is
# no terminal to drive and no display to draw on, so `--no-default-features`
# drops the terminal UI and about a third of PaperBoy's source for a runner that
# takes the same arguments and writes the same reports.
#
# This image exists because the released Linux binaries cannot serve slim
# images: they link libxml2 and glibc dynamically, and a `-slim` base has
# neither. Building the dependency in rather than asking the user to install it
# is the whole point.

FROM rust:1-bookworm AS build

# libxml2 is the one dependency that cannot be vendored -- `hurl` reaches it
# through the `libxml` crate, which links the system library and runs bindgen
# over its headers (hence libclang). libcurl and OpenSSL *are* vendored and
# built from source here, which is what perl and make are for.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        pkg-config \
        libxml2-dev \
        libclang-dev \
        perl \
        make \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

# `--locked` builds the dependency versions PaperBoy was tested against rather
# than re-resolving to whatever is newest, which matters more here than usual:
# an image is rebuilt long after the release it is named for.
RUN cargo build --release --locked --no-default-features

FROM debian:bookworm-slim

# `image.source` is what links the package to the repository on GitHub, which
# is also what makes its README and licence visible on the package page.
LABEL org.opencontainers.image.source="https://github.com/jhobern/paperboy" \
      org.opencontainers.image.description="PaperBoy headless API runner" \
      org.opencontainers.image.licenses="MIT"

# Two packages, both load-bearing:
#
#   libxml2          the binary links it dynamically; without it the process
#                    does not start at all
#   ca-certificates  the vendored OpenSSL probes the system trust store at
#                    runtime (/etc/ssl, /etc/pki/tls, ...) and a slim image
#                    ships none, so every HTTPS request would fail to verify
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libxml2 \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/paperboy /usr/local/bin/paperboy

# MIT asks that the copyright notice travel with the software, and an image is
# a distribution like any other.
COPY --from=build /src/LICENSE /usr/local/share/paperboy/LICENSE

# Collections, environments and the reports written beside them are mounted in.
# Keeping a fixed working directory means `-v "$PWD:/work"` is the whole of the
# invocation rather than something the user has to work out.
WORKDIR /work

ENTRYPOINT ["paperboy"]
CMD ["--help"]
