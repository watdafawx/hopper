# Hopper Server in a container, with Java 8, 17 and 21 for the Minecraft
# servers. Everything (servers and the panel's data) lives in one volume,
# /hopper, the panel folder.
#
#   docker build -f crates/hopperd/Dockerfile -t hopperd .
#   docker run -d --name hopperd --stop-timeout 120 \
#     -p 8790:8790 -p 8022:8022 -p 25565:25565 -v hopperd:/hopper hopperd
#
# Build from the repository root. Publish each server's game port too.
# LAN discovery (mDNS) needs --network host.
#
# Targets: the default builds hopperd from source; `--target prebuilt`
# copies an already built binary from dist/docker/<arch>/hopperd (what the
# release workflow does, one native build per architecture).

FROM node:22-bookworm-slim AS ui
WORKDIR /ui
# pnpm 9, like the release workflow (newer ones refuse esbuild's install script).
RUN npm install -g pnpm@9
COPY crates/hopperd/ui/package.json crates/hopperd/ui/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY crates/hopperd/ui/ ./
RUN pnpm build

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
COPY --from=ui /ui/dist crates/hopperd/ui/dist
RUN cargo build --release -p hopperd && cp target/release/hopperd /hopperd

# Java runtimes: Temurin JREs, one folder each.
FROM eclipse-temurin:8-jre AS java8
FROM eclipse-temurin:17-jre AS java17
FROM eclipse-temurin:21-jre AS java21

FROM ubuntu:24.04 AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates tini tzdata fontconfig \
 && rm -rf /var/lib/apt/lists/*
# Hopper looks for Java in "<ProgramFiles>/Temurin/*" and expects
# bin/java.exe; the java.exe links make the Linux JREs match that.
COPY --from=java8 /opt/java/openjdk /opt/java/Temurin/8
COPY --from=java17 /opt/java/openjdk /opt/java/Temurin/17
COPY --from=java21 /opt/java/openjdk /opt/java/Temurin/21
RUN for v in 8 17 21; do ln -s java /opt/java/Temurin/$v/bin/java.exe; done
COPY crates/hopperd/packaging/docker-entrypoint.sh /usr/local/bin/hopperd-docker
RUN sed -i 's/\r$//' /usr/local/bin/hopperd-docker && chmod +x /usr/local/bin/hopperd-docker
ENV ProgramFiles=/opt/java \
    JAVA_HOME=/opt/java/Temurin/21 \
    PATH=/opt/java/Temurin/21/bin:$PATH \
    HOPPERD_DATA=/hopper/.hopper-server \
    HOPPER_DATA_DIR=/hopper/.hopper-server/core \
    LANG=C.UTF-8
VOLUME ["/hopper"]
# Panel (HTTPS/HTTP), SFTP, server addresses (one Minecraft port for all).
EXPOSE 8790 8022 25565
STOPSIGNAL SIGTERM
ENTRYPOINT ["tini", "--", "hopperd-docker"]

FROM runtime AS prebuilt
ARG TARGETARCH
COPY dist/docker/${TARGETARCH}/hopperd /usr/local/bin/hopperd
RUN chmod +x /usr/local/bin/hopperd

FROM runtime
COPY --from=build /hopperd /usr/local/bin/hopperd
