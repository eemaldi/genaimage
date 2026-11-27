FROM docker.io/library/node:24-bookworm-slim@sha256:873f586f3794f52281bd039bf02c048a0be08fde619338f9941bad1eb6b34530 AS portal
WORKDIR /src/apps/portal
COPY apps/portal/package.json apps/portal/package-lock.json ./
RUN npm ci
COPY apps/portal ./
RUN npm run build

FROM docker.io/library/rust:1.100-bookworm@sha256:75d26c23c24cc6ea1cdabf0714a6bff8fd7bf3248716043fd7ae86dac7f41b94 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
COPY genaimage.example.toml ./
COPY --from=portal /src/apps/portal/dist apps/portal/dist
RUN cargo build --release --locked -p genaimage \
    && mkdir -p /rootfs/data/config /rootfs/data/runs /rootfs/data/uploads

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /src/target/release/genaimage /usr/local/bin/genaimage
COPY --chown=65532:65532 --from=build /rootfs/data /data
COPY workflows /usr/share/genaimage/workflows
ENV GENAIMAGE_CONTAINER=1 \
    GENAIMAGE_SERVER_HOST=0.0.0.0 \
    GENAIMAGE_SERVER_PORT=8190 \
    GENAIMAGE_STATE_DATABASE=/data/state.sqlite3 \
    GENAIMAGE_RUNS=/data/runs \
    GENAIMAGE_UPLOADS=/data/uploads \
    GENAIMAGE_PACKS=/usr/share/genaimage/workflows/packs \
    GENAIMAGE_WORKFLOWS=/data/workflows \
    HOME=/data \
    XDG_CONFIG_HOME=/data/config \
    XDG_DATA_HOME=/data
WORKDIR /data
VOLUME /data
EXPOSE 8190
USER 65532:65532
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/genaimage", "healthcheck"]
ENTRYPOINT ["/usr/local/bin/genaimage"]
CMD ["serve"]
