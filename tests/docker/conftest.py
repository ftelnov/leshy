import os
import platform
import signal
import subprocess
import tempfile
import time

import dns.resolver
import pytest

IS_MACOS = platform.system() == "Darwin"

LESHY_BIN = os.environ.get("LESHY_BIN", "/usr/local/bin/leshy")
CONFIG_DIR = os.environ.get("LESHY_CONFIG_DIR", "/etc/leshy")
LISTEN_PORT = 10053
PUBLIC_DNS = "172.28.0.10"
CORPORATE_DNS = "172.28.0.20"
BROKEN_DNS = "172.28.0.30"


@pytest.fixture(scope="session", autouse=True)
def wait_for_upstream():
    """Wait for CoreDNS to respond before any test runs."""
    for server, name in [(PUBLIC_DNS, "example.com"), (CORPORATE_DNS, "internal.company.com")]:
        resolver = dns.resolver.Resolver(configure=False)
        resolver.nameservers = [server]
        resolver.port = 53
        resolver.lifetime = 2
        for attempt in range(15):
            try:
                resolver.resolve(name, "A")
                break
            except Exception:
                if attempt == 14:
                    pytest.fail(f"Upstream {server} not ready after 30s")
                time.sleep(2)

    # Wait for broken-dns (returns REFUSED, so any DNS response means it's up)
    resolver = dns.resolver.Resolver(configure=False)
    resolver.nameservers = [BROKEN_DNS]
    resolver.port = 53
    resolver.lifetime = 2
    for attempt in range(15):
        try:
            resolver.resolve("anything.test", "A")
            break
        except (dns.resolver.NoNameservers, dns.resolver.NXDOMAIN):
            break  # REFUSED/NXDOMAIN means server is up
        except Exception:
            if attempt == 14:
                pytest.fail(f"Upstream {BROKEN_DNS} (broken-dns) not ready after 30s")
            time.sleep(2)


@pytest.fixture
def leshy():
    """Start a leshy process with the given config, stop on teardown.

    Returns a factory function: call it with a config filename.
    Accepts optional env_extra dict to override environment variables.
    The returned process has a .log_path attribute pointing to the log file.
    """
    procs = []
    log_paths = []

    def _start(config_name, env_extra=None):
        config_path = f"{CONFIG_DIR}/{config_name}"
        env = {**os.environ, "RUST_LOG": "info"}
        if env_extra:
            env.update(env_extra)

        log_path = tempfile.mktemp(suffix=".log")
        log_fd = os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o644)
        log_paths.append(log_path)

        proc = subprocess.Popen(
            [LESHY_BIN, config_path],
            env=env,
            stdout=log_fd,
            stderr=subprocess.STDOUT,
        )
        os.close(log_fd)
        proc.log_path = log_path
        procs.append(proc)
        # Wait for leshy to start listening
        time.sleep(1)
        assert proc.poll() is None, (
            f"leshy exited early: check {log_path}"
        )
        return proc

    yield _start

    for proc in procs:
        if proc.poll() is None:
            proc.send_signal(signal.SIGTERM)
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
    for path in log_paths:
        try:
            os.unlink(path)
        except OSError:
            pass


@pytest.fixture
def dns_query():
    """Query DNS via leshy."""
    def _query(name, rdtype="A", server="127.0.0.1", port=LISTEN_PORT, lifetime=5, timeout=None):
        resolver = dns.resolver.Resolver(configure=False)
        resolver.nameservers = [server]
        resolver.port = port
        resolver.lifetime = lifetime
        if timeout is not None:
            resolver.timeout = timeout
        return resolver.resolve(name, rdtype)

    return _query


@pytest.fixture
def get_routes():
    """Return current routing table as a string."""
    def _get():
        if IS_MACOS:
            result = subprocess.run(
                ["netstat", "-rn", "-f", "inet"],
                capture_output=True, text=True, check=True,
            )
        else:
            result = subprocess.run(
                ["ip", "route", "show"],
                capture_output=True, text=True, check=True,
            )
        return result.stdout

    return _get


@pytest.fixture
def dummy_interface():
    """Provide a network interface for dev-route tests, tear down after test.

    Linux: creates a dummy interface.
    macOS: uses lo0 (loopback) — no dummy interface support.
    """
    dev_file = "/tmp/vpn.dev"

    if IS_MACOS:
        iface = "lo0"
        with open(dev_file, "w") as f:
            f.write(iface)
        yield iface
        if os.path.exists(dev_file):
            os.remove(dev_file)
    else:
        iface = "dummy-vpn"
        subprocess.run(
            ["ip", "link", "add", iface, "type", "dummy"],
            check=True,
        )
        subprocess.run(
            ["ip", "link", "set", iface, "up"],
            check=True,
        )
        with open(dev_file, "w") as f:
            f.write(iface)
        yield iface
        if os.path.exists(dev_file):
            os.remove(dev_file)
        subprocess.run(
            ["ip", "link", "del", iface],
            check=False,
        )
