# dfc-node

Standalone Rust binary for DFC node operators. Runs gladiator matches on your hardware, earns $DFC rewards.

## What It Does

```
dfc-node starts → loads wallet → registers with coordinator
    ↓
spawns 3 concurrent tasks:
    1. heartbeat_loop    — sends system metrics every 60s
    2. assignment_loop   — long-polls for match assignments, spawns containers
    3. signal_handler    — SIGTERM/SIGINT → graceful shutdown
```

When a match is assigned:
1. Spawns 2 gladiator containers with Docker
2. Monitors containers (5s poll interval)
3. Detects when a container exits (flag captured / crash)
4. Reports result to coordinator
5. Cleans up containers
6. Receives $DFC reward

## Prerequisites

- Docker with `dfc-gladiator:armed` image available
- Solana wallet keypair (ed25519)
- Network access to the DFC coordinator API

## Build

```bash
cargo build --release
# Binary: target/release/dfc-node (~10MB)
```

For a fully static binary (no system dependencies):

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
# Binary: target/x86_64-unknown-linux-musl/release/dfc-node (~5MB)
```

## Configure

Copy `config.example.toml` to `config.toml`:

```toml
[coordinator]
url = "https://api.dfc.gg"

[wallet]
keypair_path = "~/.config/solana/id.json"

[docker]
image = "dfc-gladiator:armed"
max_concurrent = 2
memory_limit = 2147483648  # 2GB per container
cpu_limit = 2000000000     # 2 CPU cores per container

[hardware]
gpu = false
cpu_cores = 4
memory_gb = 16
```

Environment variable overrides:

| Variable | Overrides |
|---|---|
| `DFC_COORDINATOR_URL` | `coordinator.url` |
| `DFC_WALLET_KEYPAIR` | `wallet.keypair_path` |
| `DFC_DOCKER_IMAGE` | `docker.image` |
| `DFC_REGION` | Reported region (default: `unknown`) |

## Run

```bash
# With config file
./dfc-node --config config.toml

# With env vars
DFC_COORDINATOR_URL=https://api.dfc.gg \
DFC_WALLET_KEYPAIR=~/.config/solana/id.json \
./dfc-node
```

## Generate a Wallet

If you don't have a Solana keypair:

```bash
solana-keygen new --outfile ~/.config/solana/dfc-node.json --no-bip39-passphrase
```

## Project Structure

```
src/
├── main.rs           # Entry point — load config, register, spawn tasks
├── config.rs         # TOML config + env var overrides
├── types.rs          # API types (mirrors @dfc/common)
├── auth/
│   ├── wallet.rs     # ed25519 keypair loading + signing
│   └── token.rs      # Thread-safe JWT store
├── api/
│   └── client.rs     # HTTP client with retry + exponential backoff
├── docker/
│   └── manager.rs    # Container lifecycle (spawn, kill, inspect)
├── heartbeat.rs      # 60s interval loop, /proc metrics
├── assignments.rs    # Long-poll → spawn → monitor → report
└── shutdown.rs       # SIGTERM/SIGINT graceful shutdown
```

## Test

```bash
cargo test      # 8 unit tests
cargo clippy    # lint
```

## Security

- Containers run with `--cap-drop=ALL` and `--no-new-privileges`
- Memory and CPU limits enforced per container
- PID limit of 512 per container
- All containers labeled with `dfc.matchId` and `dfc.slot` for cleanup
- Graceful shutdown finishes current matches before exiting

## Related

- [dfc](https://github.com/your-org/dfc) — Coordinator API + frontend
- [dfc-sdk](https://github.com/your-org/dfc-sdk) — TypeScript SDK + OpenAPI spec

## License

MIT
