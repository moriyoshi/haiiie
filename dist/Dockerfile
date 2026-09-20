# Build from the haiiie repository root:
#   docker build -f dist/Dockerfile -t haiiie:local .
# The sibling yesno revision is fetched from the public repository at the
# exact commit recorded in YESNO_REVISION, which Cargo's path dependencies use.
ARG RUST_VERSION=1.95
FROM rust:${RUST_VERSION}-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends git && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY YESNO_REVISION /src/YESNO_REVISION
RUN git clone https://github.com/moriyoshi/yesnodb.git /src/yesno && \
    git -C /src/yesno reset --hard "$(cat /src/YESNO_REVISION)" && \
    test "$(git -C /src/yesno rev-parse HEAD)" = "$(cat /src/YESNO_REVISION)"
COPY . /src/haiiie
WORKDIR /src/haiiie
RUN cargo build --release --locked -p haiiie-cli

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 10001 haiiie && \
    useradd --uid 10001 --gid 10001 --no-create-home haiiie
COPY --from=build /src/haiiie/target/release/haiiied /usr/local/bin/haiiied
COPY --from=build /src/haiiie/target/release/haiiie /usr/local/bin/haiiie
USER 10001:10001
EXPOSE 50071
ENTRYPOINT ["/usr/local/bin/haiiied"]
