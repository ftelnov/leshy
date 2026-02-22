.PHONY: help build test integration-test watch check fmt clippy clean install run

# Default target
help:
	@echo "Leshy Development Commands:"
	@echo ""
	@echo "  make test             - Run fmt check, clippy, and unit tests"
	@echo "  make integration-test - Run Docker integration tests"
	@echo "  make watch            - Watch for changes and auto-test (requires entr)"
	@echo "  make build   - Build release binary"
	@echo "  make check   - Run cargo check"
	@echo "  make fmt     - Format code"
	@echo "  make clippy  - Run clippy lints"
	@echo "  make clean   - Clean build artifacts"
	@echo "  make install - Install to /usr/local/bin (requires sudo)"
	@echo "  make run     - Run with example config"
	@echo ""

# Build release binary
build:
	@echo "Building release binary..."
	cargo build --release
	@echo "Binary: ./target/release/leshy"
	@ls -lh ./target/release/leshy

# Run fmt check, clippy, and unit tests (CI-friendly, no watch)
test:
	cargo fmt -- --check
	cargo clippy --all-targets --all-features -- -D warnings
	cargo test

# Run Docker integration tests
integration-test:
	docker compose -f tests/docker/docker-compose.yml up --build --abort-on-container-exit --exit-code-from test-runner
	docker compose -f tests/docker/docker-compose.yml down

# Watch for changes and auto-test (requires entr)
watch:
	@./watch.sh test

# Quick check (no tests)
check:
	@echo "Running cargo check..."
	cargo check --all-targets

# Format code
fmt:
	@echo "Formatting code..."
	cargo fmt

# Run clippy
clippy:
	@echo "Running clippy..."
	cargo clippy --all-targets --all-features -- -D warnings

# Clean build artifacts
clean:
	@echo "Cleaning build artifacts..."
	cargo clean
	@rm -f test-config.toml

# Install system-wide (requires sudo)
install: build
	@echo "Installing to /usr/local/bin..."
	@sudo cp target/release/leshy /usr/local/bin/
	@sudo chmod +x /usr/local/bin/leshy
	@echo "Installed: /usr/local/bin/leshy"

# Run with config (elevates only the binary, never the build)
run: build
	sudo RUST_LOG=info ./target/release/leshy $(or $(CONFIG),config.example.toml)
