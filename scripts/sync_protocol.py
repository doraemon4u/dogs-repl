#!/usr/bin/env python3
"""
OGS Realtime Protocol Synchronization & Compatibility Sentinel (Design Section 5, Option A).

This script compares the official OGS Realtime protocol definitions from `online-go/goban`
(ClientToServer.ts and ServerToClient.ts) against the Rust Transport layer in
`src/transport/realtime_client.rs`.

Usage:
    conda run -n universe python scripts/sync_protocol.py
    conda run -n universe python scripts/sync_protocol.py --check
    conda run -n universe python scripts/sync_protocol.py --json
    conda run -n universe python scripts/sync_protocol.py --update-cache
"""

import argparse
import json
import os
import re
import sys
import urllib.request
from pathlib import Path
from typing import Dict, List, Set, Tuple

REPO_ROOT = Path(__file__).resolve().parent.parent
CACHE_DIR = Path(__file__).resolve().parent / ".protocol_cache"

GOBAN_RAW_BASE = "https://raw.githubusercontent.com/online-go/goban/main/src/engine/protocol"
CLIENT_TO_SERVER_URL = f"{GOBAN_RAW_BASE}/ClientToServer.ts"
SERVER_TO_CLIENT_URL = f"{GOBAN_RAW_BASE}/ServerToClient.ts"

RUST_REALTIME_CLIENT = REPO_ROOT / "src" / "transport" / "realtime_client.rs"

# Commands that are intentionally omitted from a human interactive REPL/TUI
# (e.g., bot services, automatch matchmaking queues, seek-graph lobby browsers, AI workers)
# as documented in manuals/OGS_repl_process.md line 70.
KNOWN_EXCLUDED_COMMANDS = {
    "automatch/available/subscribe",
    "automatch/available/unsubscribe",
    "automatch/cancel",
    "automatch/find_match",
    "automatch/list",
    "bot/config",
    "bot/status",
    "itc",
    "net/timeout",
    "net/unrecoverable_error",
    "remote_storage/remove",
    "remote_storage/set",
    "remote_storage/sync",
    "seek_graph/connect",
    "seek_graph/disconnect",
    "ui-pushes/subscribe",
    "ui-pushes/unsubscribe",
}

# Events that are web-only (inter-tab, moderation reports, seekgraph browser, automatch UI)
KNOWN_EXCLUDED_EVENTS = {
    "HUP",
    "active-bots",
    "automatch/available/add",
    "automatch/available/remove",
    "automatch/cancel",
    "automatch/entry",
    "automatch/start",
    "chat-join",
    "chat-message-removed",
    "chat-part",
    "chat-topic",
    "chat-update-user",
    "config/last_game",
    "incident-report",
    "incident-report-sync-complete",
    "itc",
    "remote_storage/sync_complete",
    "remote_storage/update",
    "score-estimator-enabled-state",
    "seekgraph/global",
    "ui-push",
    "user/jwt",
    "user/state",
}


def fetch_url(url: str, cache_file: Path, update_cache: bool = False) -> str:
    """Fetches text from URL with offline fallback cache."""
    if not update_cache and cache_file.exists():
        try:
            return cache_file.read_text(encoding="utf-8")
        except Exception:
            pass

    try:
        req = urllib.request.Request(
            url,
            headers={"User-Agent": "ogs-repl-sync-script/1.0"},
        )
        with urllib.request.urlopen(req, timeout=10) as resp:
            content = resp.read().decode("utf-8")
            CACHE_DIR.mkdir(parents=True, exist_ok=True)
            cache_file.write_text(content, encoding="utf-8")
            return content
    except Exception as e:
        if cache_file.exists():
            print(f"[Warning] Failed to fetch {url} ({e}); using cached copy.", file=sys.stderr)
            return cache_file.read_text(encoding="utf-8")
        raise RuntimeError(f"Failed to fetch {url} and no cache exists: {e}") from e


def extract_interface_methods(content: str, interface_name: str) -> Set[str]:
    """Extracts method names from an interface block in TypeScript protocol definitions."""
    idx = content.find(interface_name)
    if idx == -1:
        return set()
    block = content[idx:]
    # Match entries like: "command/name": (
    matches = re.findall(r"""^\s{4}["\']([a-zA-Z0-9_\/\-]+)["\']\s*:\s*\(""", block, re.MULTILINE)
    return set(matches)


def parse_rust_realtime_protocol(rust_source: str) -> Tuple[Set[str], Set[str]]:
    """
    Parses commands and events implemented in src/transport/realtime_client.rs.
    """
    commands: Set[str] = set()
    events: Set[str] = set()

    # Commands issued via inner.request("command", ...) or inner.notify("command", ...)
    cmd_pattern = re.compile(r"""inner\s*\.\s*(?:request|notify)\s*\(\s*["\']([^"\']+)["\']""")
    for m in cmd_pattern.finditer(rust_source):
        commands.add(m.group(1))

    # Also capture authenticate / ping emits
    emit_pattern = re.compile(r"""client\s*\.\s*emit\s*\(\s*["\']([^"\']+)["\']""")
    for m in emit_pattern.finditer(rust_source):
        commands.add(m.group(1))

    # Events matched in push_event
    event_pattern = re.compile(r"""["\']([a-zA-Z0-9_\/\-]+)["\']\s*(?:\||\=>)""")
    for m in event_pattern.finditer(rust_source):
        events.add(m.group(1))

    if 'starts_with("game/")' in rust_source:
        events.add("game/*")
    if 'ends_with("/chat")' in rust_source:
        events.add("*/chat")

    # Socket-level registered events (e.g. .on("error", ...))
    on_pattern = re.compile(r"""\.on\s*\(\s*["\']([^"\']+)["\']""")
    for m in on_pattern.finditer(rust_source):
        name = m.group(1)
        if name == "error":
            events.add("ERROR")
        else:
            events.add(name)

    return commands, events


def main() -> int:
    parser = argparse.ArgumentParser(description="OGS Protocol Compatibility Sentinel")
    parser.add_argument("--check", action="store_true", help="Exit with non-zero status if unhandled commands exist")
    parser.add_argument("--json", action="store_true", help="Output summary as JSON")
    parser.add_argument("--update-cache", action="store_true", help="Force update the local protocol cache")
    parser.add_argument("--client-file", type=Path, help="Path to local ClientToServer.ts")
    parser.add_argument("--server-file", type=Path, help="Path to local ServerToClient.ts")
    parser.add_argument("--rust-file", type=Path, default=RUST_REALTIME_CLIENT, help="Path to realtime_client.rs")
    args = parser.parse_args()

    # 1. Load Goban TypeScript protocol
    if args.client_file and args.client_file.exists():
        client_ts = args.client_file.read_text(encoding="utf-8")
    else:
        cache_client = CACHE_DIR / "ClientToServer.ts"
        client_ts = fetch_url(CLIENT_TO_SERVER_URL, cache_client, update_cache=args.update_cache)

    if args.server_file and args.server_file.exists():
        server_ts = args.server_file.read_text(encoding="utf-8")
    else:
        cache_server = CACHE_DIR / "ServerToClient.ts"
        server_ts = fetch_url(SERVER_TO_CLIENT_URL, cache_server, update_cache=args.update_cache)

    goban_commands = extract_interface_methods(client_ts, "interface ClientToServer")
    goban_events = extract_interface_methods(server_ts, "interface ServerToClient")

    # 2. Load Rust implementation
    if not args.rust_file.exists():
        print(f"Error: Rust source file not found at {args.rust_file}", file=sys.stderr)
        return 2

    rust_source = args.rust_file.read_text(encoding="utf-8")
    rust_commands, rust_events = parse_rust_realtime_protocol(rust_source)

    # 3. Diff commands
    implemented_commands = goban_commands.intersection(rust_commands)
    missing_commands = goban_commands.difference(rust_commands)
    expected_missing_commands = missing_commands.intersection(KNOWN_EXCLUDED_COMMANDS)
    unexpected_missing_commands = missing_commands.difference(KNOWN_EXCLUDED_COMMANDS)

    # 4. Diff events
    implemented_events = set()
    for ev in goban_events:
        if ev in rust_events:
            implemented_events.add(ev)
        elif "game/*" in rust_events and ev.startswith("game/"):
            implemented_events.add(ev)
        elif "*/chat" in rust_events and ev.endswith("/chat"):
            implemented_events.add(ev)

    missing_events = goban_events.difference(implemented_events)
    expected_missing_events = missing_events.intersection(KNOWN_EXCLUDED_EVENTS)
    unexpected_missing_events = missing_events.difference(KNOWN_EXCLUDED_EVENTS)

    report = {
        "commands": {
            "total_goban": len(goban_commands),
            "implemented_count": len(implemented_commands),
            "implemented": sorted(implemented_commands),
            "expected_missing": sorted(expected_missing_commands),
            "unexpected_missing": sorted(unexpected_missing_commands),
        },
        "events": {
            "total_goban": len(goban_events),
            "implemented_count": len(implemented_events),
            "implemented": sorted(implemented_events),
            "expected_missing": sorted(expected_missing_events),
            "unexpected_missing": sorted(unexpected_missing_events),
        },
    }

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        print("=== OGS Protocol Synchronization Report (online-go/goban vs ogs-tui) ===")
        print(f"\n[Commands (Client -> Server)]")
        print(f"  Total in Goban Protocol:    {len(goban_commands)}")
        print(f"  Implemented in Rust Client: {len(implemented_commands)}")
        print(f"  Expected Non-REPL Excluded: {len(expected_missing_commands)}")
        if unexpected_missing_commands:
            print(f"  [!] UNEXPECTED MISSING COMMANDS ({len(unexpected_missing_commands)}):")
            for cmd in sorted(unexpected_missing_commands):
                print(f"      - {cmd}")
        else:
            print("  All interactive REPL commands are covered (100% of target)!")

        print(f"\n[Events (Server -> Client)]")
        print(f"  Total in Goban Protocol:    {len(goban_events)}")
        print(f"  Handled in Rust Client:     {len(implemented_events)}")
        print(f"  Expected Non-REPL Excluded: {len(expected_missing_events)}")
        if unexpected_missing_events:
            print(f"  [!] UNEXPECTED MISSING EVENTS ({len(unexpected_missing_events)}):")
            for ev in sorted(unexpected_missing_events):
                print(f"      - {ev}")
        else:
            print("  All relevant interactive events are handled (100% of target)!")

    if args.check:
        if unexpected_missing_commands or unexpected_missing_events:
            return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
