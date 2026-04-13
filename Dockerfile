# ============================================
# CURS3D - Multi-stage Docker Build
# Quantum-Resistant Layer 1 Blockchain
# ============================================

# --- Builder Stage ---
FROM rust:1.94 AS builder

WORKDIR /usr/src/curs3d

# Copy manifests for dependency caching
COPY Cargo.toml Cargo.lock ./

# Create dummy src to build dependencies first
RUN mkdir src && \
    echo "fn main() { println!(\"dummy\"); }" > src/main.rs

# Build only dependencies (cached layer)
RUN cargo build --release && \
    rm -rf src

# Copy real source code
COPY src ./src

# Touch main.rs to invalidate the binary but not deps
RUN touch src/main.rs

# Build the actual binary
RUN cargo build --release

# --- Runtime Stage ---
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*

# Copy the compiled binary
COPY --from=builder /usr/src/curs3d/target/release/curs3d /usr/local/bin/curs3d

# P2P port
EXPOSE 4337
# TCP RPC port
EXPOSE 9545
# HTTP API port
EXPOSE 8080

ENTRYPOINT ["curs3d"]
CMD ["node"]
