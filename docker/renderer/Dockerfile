# agent-browser-renderer: shared screenshot renderer for Lightpanda agents.
#
# Build from the repository root:
#   docker build -f docker/renderer/Dockerfile -t agent-browser-renderer .
# Run:
#   docker run --rm -p 9300:9300 -e AGENT_BROWSER_RENDERER_TOKEN=change-me agent-browser-renderer
# Point agents at it:
#   agent-browser --screenshot-renderer http://renderer:9300 screenshot

FROM rust:1-bookworm AS build
WORKDIR /src
COPY cli ./cli
# The dashboard is embedded at compile time; the renderer does not serve it.
RUN mkdir -p packages/dashboard/out \
    && echo '<!doctype html><title>agent-browser</title>' > packages/dashboard/out/index.html
RUN cd cli && cargo build --release --locked --bin agent-browser

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        chromium \
        ca-certificates \
        fonts-liberation \
        fonts-noto-core \
        fonts-noto-cjk \
        fonts-noto-color-emoji \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 renderer
COPY --from=build /src/cli/target/release/agent-browser /usr/local/bin/agent-browser

USER renderer
ENV AGENT_BROWSER_EXECUTABLE_PATH=/usr/bin/chromium \
    AGENT_BROWSER_RENDERER_HOST=0.0.0.0 \
    AGENT_BROWSER_RENDERER_PORT=9300 \
    AGENT_BROWSER_RENDERER_CONCURRENCY=4
EXPOSE 9300
ENTRYPOINT ["agent-browser", "renderer", "serve"]
