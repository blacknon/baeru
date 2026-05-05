# syntax=docker/dockerfile:1.7

FROM rust:1-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY themes ./themes
COPY examples ./examples
COPY README.md LICENSE ./

RUN cargo build --locked --release

FROM debian:bookworm-slim AS runtime

ENV TERM=xterm-256color \
    COLORTERM=truecolor \
    LANG=C.UTF-8 \
    LC_ALL=C.UTF-8

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        ffmpeg \
        git \
        gnupg \
        htop \
        neofetch \
        procps \
        curl \
        wget \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /etc/apt/keyrings \
    && curl -fsSL https://repo.charm.sh/apt/gpg.key \
        | gpg --dearmor -o /etc/apt/keyrings/charm.gpg \
    && echo "deb [signed-by=/etc/apt/keyrings/charm.gpg] https://repo.charm.sh/apt/ * *" \
        > /etc/apt/sources.list.d/charm.list \
    && apt-get update \
    && apt-get install -y --no-install-recommends vhs \
    && rm -rf /var/lib/apt/lists/*

RUN if apt-cache show fastfetch >/dev/null 2>&1; then \
        apt-get update \
        && apt-get install -y --no-install-recommends fastfetch \
        && rm -rf /var/lib/apt/lists/*; \
    fi

RUN useradd -m -u 1000 -s /bin/bash baeru

WORKDIR /workspace

COPY --from=builder /app/target/release/baeru /usr/local/bin/baeru
COPY themes ./themes
COPY examples ./examples
COPY baeru.yml ./baeru.yml
COPY README.md LICENSE ./

USER baeru

CMD ["baeru", "--help"]
