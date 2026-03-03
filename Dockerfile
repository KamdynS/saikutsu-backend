# Build stage
FROM rust:latest AS builder

WORKDIR /app

# Install build dependencies (cmake needed for rusqlite bundled build)
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Create dummy src to cache dependencies
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release
RUN rm -rf src

# Copy actual source code
COPY src ./src
COPY migrations ./migrations
COPY data ./data

# Build the actual application
RUN touch src/main.rs && cargo build --release

# Runtime stage
FROM debian:bookworm-slim

WORKDIR /app

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    ffmpeg \
    && rm -rf /var/lib/apt/lists/*

# Copy the binary
COPY --from=builder /app/target/release/saikutsu /app/saikutsu

# Copy migrations for runtime
COPY --from=builder /app/migrations /app/migrations

# Copy data files (dictionaries)
COPY --from=builder /app/data /app/data

EXPOSE 8080

CMD ["/app/saikutsu"]
