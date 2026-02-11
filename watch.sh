#!/usr/bin/env bash
#
# Simple watch script using entr
# Watches Rust source files and re-runs tests on changes
#
# Usage:
#   sudo ./watch.sh test          # All tests (unit + integration, requires root)
#   ./watch.sh unit               # Unit tests only (no root needed)
#   ./watch.sh integration        # Integration tests only (requires root)
#   ./watch.sh check              # Quick check
#   ./watch.sh build              # Build release
#   ./watch.sh clippy             # Lints

set -uo pipefail  # Removed -e so errors don't exit

# Change to script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'
BOLD='\033[1m'

# Check if entr is installed
if ! command -v entr &> /dev/null; then
    echo -e "${RED}Error: entr not found${NC}"
    echo "Install with:"
    echo "  Ubuntu/Debian: sudo apt install entr"
    echo "  macOS: brew install entr"
    echo "  Arch: sudo pacman -S entr"
    exit 1
fi

# Command to run (default: test)
CMD="${1:-test}"

print_header() {
    echo ""
    echo -e "${BOLD}${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    echo -e "${BOLD}${BLUE}  $1 - $(date '+%Y-%m-%d %H:%M:%S')${NC}"
    echo -e "${BOLD}${BLUE}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
}

print_footer() {
    local exit_code=$1
    echo ""
    if [ $exit_code -eq 0 ]; then
        echo -e "${BOLD}${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
        echo -e "${BOLD}${GREEN}  ✓ SUCCESS${NC}"
        echo -e "${BOLD}${GREEN}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    else
        echo -e "${BOLD}${RED}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}" >&2
        echo -e "${BOLD}${RED}  ✗ FAILED (exit code: $exit_code)${NC}" >&2
        echo -e "${BOLD}${RED}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}" >&2
    fi
}

run_command() {
    local exit_code=0

    case "$CMD" in
        test)
            print_header "Running All Tests"
            cargo test --color=always 2>&1 || exit_code=$?
            print_footer $exit_code
            ;;
        unit)
            print_header "Running Unit Tests Only"
            cargo test --lib --color=always 2>&1 || exit_code=$?
            print_footer $exit_code
            ;;
        integration)
            print_header "Running Integration Tests Only"
            cargo test --test integration_test --color=always 2>&1 || exit_code=$?
            print_footer $exit_code
            ;;
        check)
            print_header "Checking"
            cargo check --color=always 2>&1 || exit_code=$?
            print_footer $exit_code
            ;;
        build)
            print_header "Building"
            cargo build --release --color=always 2>&1 || exit_code=$?
            print_footer $exit_code
            ;;
        clippy)
            print_header "Running Clippy"
            cargo clippy --all-targets --all-features --color=always 2>&1 || exit_code=$?
            print_footer $exit_code
            ;;
        all)
            print_header "Full Test Suite"
            cargo fmt --check 2>&1 || true
            cargo clippy --all-targets --all-features --color=always 2>&1 || true
            cargo test --color=always 2>&1 || exit_code=$?
            print_footer $exit_code
            ;;
        *)
            echo -e "${RED}Unknown command: $CMD${NC}" >&2
            echo "Available: test, unit, integration, check, build, clippy, all" >&2
            return 1
            ;;
    esac

    # Always return 0 so entr keeps running
    return 0
}

echo -e "${GREEN}👁  Watching for changes...${NC}"
echo -e "${BLUE}Command: ${BOLD}$CMD${NC}"
echo -e "${YELLOW}Press Ctrl+C to stop${NC}"

# Run once at startup
run_command || true

echo ""
echo -e "${BOLD}${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${GREEN}👁  Watching for changes...${NC}"
echo -e "${BOLD}${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"

# Watch Rust source files and Cargo.toml
# -c: clear screen before running
# -r: reload entr if it exits (for when new files are added)
# Note: We export functions and always return 0 so entr never exits
find src tests Cargo.toml Cargo.lock -type f 2>/dev/null | \
    entr -c -r bash -c "
        $(declare -f print_header)
        $(declare -f print_footer)
        $(declare -f run_command)
        CMD='$CMD'
        RED='$RED'
        GREEN='$GREEN'
        YELLOW='$YELLOW'
        BLUE='$BLUE'
        NC='$NC'
        BOLD='$BOLD'
        run_command || true
        exit 0
    "
