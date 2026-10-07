.PHONY: all build release test check run install package publish docker clean

# Default target
all: build test

# Build debug binary
build:
	cargo build

# Build optimized release binary
release:
	cargo build --release

# Run unit and integration tests
test:
	cargo test

# Check formatting, linter, and compilation
check:
	cargo check
	cargo clippy --all-targets --all-features

# Run with local config.toml
run:
	cargo run --bin dnsproxy -- --config config.toml

# Install dnsproxy binary to ~/.cargo/bin
install:
	cargo install --path .

# Validate and create crates.io package
package:
	cargo package

# Dry-run publish to crates.io
check-publish:
	cargo publish --dry-run

# Publish to crates.io
publish:
	cargo publish

# Build Docker container image
docker:
	docker build -t dnsproxy:latest .

# Run Docker container
docker-run:
	docker compose up -d

# Clean build artifacts
clean:
	cargo clean
