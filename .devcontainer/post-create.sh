#!/bin/bash
set -e

echo "🚀 ThoughtGate DevContainer Post-Create Setup"
echo "=============================================="

# Configure git safe directory
echo "📁 Configuring git safe directory..."
# Host .gitconfig is mounted read-only, so use system-level config
sudo git config --system --add safe.directory /workspaces/thoughtgate || \
    echo "⚠️  Could not set git safe directory (may already be configured)"

# Install Rust nightly (required for cargo-fuzz)
echo "🦀 Installing Rust nightly toolchain..."
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly

# Install cargo development tools
echo "🔧 Installing cargo development tools..."

# cargo-fuzz (L3 Verification - Adversarial Testing)
echo "  - cargo-fuzz (adversarial testing)..."
cargo install cargo-fuzz

# cargo-nextest (faster test runner)
echo "  - cargo-nextest (faster test execution)..."
cargo install cargo-nextest --locked

# cargo-watch (development workflow)
echo "  - cargo-watch (auto-rebuild)..."
cargo install cargo-watch

# cargo-audit (security vulnerability scanning)
echo "  - cargo-audit (security auditing)..."
cargo install cargo-audit --locked

# cargo-insta (snapshot testing)
echo "  - cargo-insta (snapshot testing)..."
cargo install cargo-insta --locked

# mantra (L1 Verification - Traceability)
echo "  - mantra (traceability checking)..."
cargo install mantra || echo "⚠️  mantra installation failed (known issue, will retry later)"

# Install system dependencies for profiling
echo "🔍 Installing profiling tools..."
sudo apt-get update -qq
sudo apt-get install -y -qq time valgrind linux-perf 2>/dev/null || echo "⚠️  Some profiling tools may not be available"

# Pre-download project dependencies
echo "📦 Pre-downloading project dependencies..."
cargo fetch

# Build project in debug mode to cache dependencies
echo "🏗️  Building project (this may take a few minutes)..."
cargo build --all-features

# Run tests to ensure everything works
echo "🧪 Running test suite to verify setup..."
set -o pipefail
cargo test --no-fail-fast 2>&1 | tail -20

# Display installed tool versions
echo ""
echo "✅ DevContainer Setup Complete!"
echo "================================"
echo ""
echo "Installed Versions:"
echo "-------------------"
rustc --version
cargo --version
echo "Nightly: $(rustup toolchain list | grep nightly || echo 'not installed')"
echo "cargo-fuzz: $(cargo fuzz --version 2>&1 | head -1 || echo 'not installed')"
echo "cargo-nextest: $(cargo nextest --version 2>&1 || echo 'not installed')"
echo "cargo-watch: $(cargo watch --version 2>&1 || echo 'not installed')"
echo "cargo-audit: $(cargo audit --version 2>&1 || echo 'not installed')"
echo "cargo-insta: $(cargo insta --version 2>&1 || echo 'not installed')"
echo "mantra: $(mantra --version 2>&1 || echo 'not installed')"
echo ""
echo "Verification Hierarchy Available:"
echo "----------------------------------"
echo "L0 (Functional): cargo test"
echo "L1 (Traceability): mantra check (pending config)"
echo "L2 (Property): cargo test --test prop_*"
echo "L3 (Fuzzing): cargo +nightly fuzz run <target>"
echo "L4 (Idiomatic): cargo clippy -- -D warnings"
echo ""
echo "Memory Profiling:"
echo "-----------------"
echo "/usr/bin/time -v cargo test --test memory_profile -- --nocapture"
echo ""
echo "🎉 Ready to develop ThoughtGate!"

