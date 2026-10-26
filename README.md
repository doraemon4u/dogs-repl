# dogs-repl

Terminal REPL / TUI client for [Online-Go.com](https://online-go.com) (OGS) written in Rust.

## Features

- **Pure Rust Engine**: Board state, Go rules (liberties, capture, ko detection, suicide prevention), SGF parsing/formatting, and territory estimation.
- **Async Session Layer**: Single-owner Actor model (`tokio`), monotonic store state, real-time event distribution, and official scoring agreement synchronization.
- **Transport Layer**:
  - Full REST client with exponential backoff on HTTP 429/5xx.
  - Socket.IO / WebSocket Realtime client matching the official `online-go/goban` protocol.
  - Heartbeat (`net/ping` / `net/pong`) and automatic reconnect with state preservation.
- **Terminal UI**:
  - Interactive ratatui TUI with responsive board layout, dynamic resizing, and ANSI/256/Truecolor support.
  - Text-based REPL fallback mode.
  - Local territory estimation overlay with dead stone toggling.
- **Protocol Compatibility Sentinel**:
  - `scripts/sync_protocol.py` continuously compares OGS protocol definitions against the Rust transport layer.

## Quick Start

### Build and Run

```bash
# Build release binary
cargo build --release

# Run unit tests
cargo test
```

### Protocol Synchronization

```bash
conda run -n universe python scripts/sync_protocol.py --check
```

## Documentation

Comprehensive architecture, protocol analysis, and progress documentation are located in `manuals/`:
- `manuals/OGS_REPL.md`: Architecture design and protocol specifications.
- `manuals/OGS_repl_process.md`: Multi-stage development roadmap and progress.
- `manuals/AUDIT.md`: Architecture consistency and platform audit report.

## License

MIT OR Apache-2.0
