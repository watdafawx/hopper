# Hopper Server in a container, with Java 21 for the Minecraft servers.
#
#   docker build -f crates/hopperd/Dockerfile -t hopperd .
#   docker run -d --name hopperd -p 8790:8790 -p 8022:8022 -p 25565:25565 \
#     -v hopperd-data:/data -v /srv/minecraft:/servers hopperd
#
# Build from the repository root. Publish each server's game port too.
# LAN discovery (mDNS) needs --network host.

FROM node:22-bookworm-slim AS ui
WORKDIR /ui
RUN corepack enable
COPY crates/hopperd/ui/package.json crates/hopperd/ui/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY crates/hopperd/ui/ ./
RUN pnpm build

FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
COPY --from=ui /ui/dist crates/hopperd/ui/dist
RUN cargo build --release -p hopperd

FROM eclipse-temurin:21-jre
COPY --from=build /src/target/release/hopperd /usr/local/bin/hopperd
ENV HOPPERD_DATA=/data
VOLUME ["/data", "/servers"]
EXPOSE 8790 8022
ENTRYPOINT ["hopperd", "--servers-root", "/servers"]
