# SPDX-License-Identifier: BUSL-1.1
#
# Image Docker du sidecar REST `basemyai-rest` (docs/status.md: "image Docker REST").
#
# Build context = RACINE du workspace (pas ce dossier), car basemyai-rest dépend
# des autres crates du workspace Cargo :
#
#   docker build -f crates/basemyai-rest/Dockerfile -t basemyai-rest:latest .
#
# Le backend est 100% Rust natif (basemyai-engine, ADR-024/025/030) : pas de
# CMake, pas de libSQL, pas de dépendance SQLite. Le seul besoin de toolchain
# C est `tokenizers` (feature "onig", oniguruma vendée en C) → build-essential
# dans le stage builder uniquement.
#
# L'embedder (Candle, feature "embed", activée par défaut) NE télécharge
# JAMAIS le modèle silencieusement (ADR-010) : le modèle baseline
# `all-MiniLM-L6-v2` (config.json, tokenizer.json, model.safetensors) doit
# être fourni via un volume monté sur BASEMYAI_MODEL_PATH, sinon le
# conteneur refuse de démarrer (sauf BASEMYAI_FETCH=1, qui autorise le
# téléchargement HuggingFace explicite au démarrage — voir README du
# volume `model` dans docker-compose.yml).

# ---- Stage 1: builder ------------------------------------------------------
FROM rust:1.95-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace

# Copie tout le workspace (nécessaire : basemyai-rest dépend de basemyai,
# basemyai-core et basemyai-engine via `path = "../..."`). .dockerignore
# exclut target/, .git, les modèles, etc.
COPY . .

# Build release du seul binaire basemyai-rest (feature `embed` par défaut,
# cf. crates/basemyai-rest/Cargo.toml).
RUN cargo build --release -p basemyai-rest --features embed

# ---- Stage 2: runtime -------------------------------------------------------
FROM debian:bookworm-slim AS runtime

# ca-certificates : requis par reqwest/rustls pour le fetch HuggingFace
# explicite (BASEMYAI_FETCH=1) et les appels sortants de détection LLM.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin basemyai

COPY --from=builder /workspace/target/release/basemyai-rest /usr/local/bin/basemyai-rest

# Volumes attendus :
#   /data/memory.bmai    ← BASEMYAI_DB_PATH (conteneur natif chiffré)
#   /data/model          ← BASEMYAI_MODEL_PATH (config.json, tokenizer.json,
#                          model.safetensors du baseline all-MiniLM-L6-v2)
RUN mkdir -p /data/model && chown -R basemyai:basemyai /data
USER basemyai
WORKDIR /data

ENV BASEMYAI_REST_BIND=0.0.0.0 \
    BASEMYAI_REST_PORT=7743 \
    BASEMYAI_DB_PATH=/data/memory.bmai \
    BASEMYAI_MODEL_PATH=/data/model

EXPOSE 7743

ENTRYPOINT ["/usr/local/bin/basemyai-rest"]
