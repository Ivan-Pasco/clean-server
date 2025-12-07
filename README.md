# Clean Server

High-performance HTTP server runtime for executing Clean Language WebAssembly applications.

## Overview

Clean Server is a production-ready HTTP server that executes compiled Clean Language (`.wasm`) applications. It provides a complete runtime environment with:

- **WebAssembly Execution**: Fast, secure sandboxed execution using Wasmtime
- **HTTP Server**: High-performance async HTTP server built on Axum
- **Host Bridge**: Comprehensive system integration (HTTP, DB, FS, Crypto, etc.)
- **Multi-Platform**: Runs on Linux, macOS, and Windows (x64 and ARM64)

## Installation

### Pre-built Binaries

Download the latest release for your platform:

```bash
# Linux x86_64
curl -L https://github.com/Ivan-Pasco/clean-server/releases/latest/download/clean-server-linux-x86_64.tar.gz | tar xz

# Linux ARM64
curl -L https://github.com/Ivan-Pasco/clean-server/releases/latest/download/clean-server-linux-arm64.tar.gz | tar xz

# macOS x86_64 (Intel)
curl -L https://github.com/Ivan-Pasco/clean-server/releases/latest/download/clean-server-macos-x86_64.tar.gz | tar xz

# macOS ARM64 (Apple Silicon)
curl -L https://github.com/Ivan-Pasco/clean-server/releases/latest/download/clean-server-macos-arm64.tar.gz | tar xz

# Windows x86_64
# Download from: https://github.com/Ivan-Pasco/clean-server/releases/latest/download/clean-server-windows-x86_64.exe.zip
```

### Build from Source

```bash
git clone https://github.com/Ivan-Pasco/clean-server.git
cd clean-server
cargo build --release
```

The binary will be available at `target/release/clean-server`.

## Quick Start

### Basic Usage

```bash
# Run a Clean Language WASM application
clean-server run app.wasm

# Specify port
clean-server run app.wasm --port 8080

# Enable debug logging
clean-server run app.wasm --log-level debug
```

### Command Line Options

```
clean-server run [OPTIONS] <WASM_FILE>

Arguments:
  <WASM_FILE>  Path to the compiled WASM file

Options:
  -p, --port <PORT>              Server port [default: 3000]
  -H, --host <HOST>              Server host [default: 0.0.0.0]
      --log-level <LEVEL>        Log level [default: info]
                                 [possible values: trace, debug, info, warn, error]
      --max-memory <MB>          Maximum WASM memory in MB [default: 512]
      --max-request-size <MB>    Maximum request body size in MB [default: 10]
      --cors                     Enable CORS
      --cors-origin <ORIGIN>     CORS allowed origin [default: *]
  -h, --help                     Print help
  -V, --version                  Print version
```

### Example Application

Create a simple Clean Language application:

```clean
// hello.cln
endpoints:
	GET "/":
		returns: json({
			message: "Hello from Clean Server!",
			timestamp: time.now()
		})

	GET "/health":
		returns: json({ status: "ok" })
```

Compile and run:

```bash
# Compile with Clean Language compiler
clean-compile hello.cln -o hello.wasm

# Run with Clean Server
clean-server run hello.wasm
```

Test the endpoints:

```bash
curl http://localhost:3000/
curl http://localhost:3000/health
```

## Host Bridge Capabilities

Clean Server provides comprehensive system integration through the Host Bridge:

### HTTP Client (`bridge:http`)
- Make HTTP requests (GET, POST, PUT, DELETE, etc.)
- Handle headers, query parameters, and request bodies
- Support for JSON, form data, and multipart uploads

### Database (`bridge:db`)
- PostgreSQL, MySQL, SQLite support
- Parameterized queries for SQL injection prevention
- Connection pooling and transaction support

### Environment (`bridge:env`)
- Read environment variables
- Access system configuration
- Secure credential management

### Time (`bridge:time`)
- Current timestamp and date/time operations
- Timezone handling
- Duration and interval calculations

### Cryptography (`bridge:crypto`)
- Password hashing (bcrypt, argon2)
- JWT token generation and validation
- Random number generation
- SHA-256 hashing

### Logging (`bridge:log`)
- Structured logging (trace, debug, info, warn, error)
- JSON log output
- Contextual logging with metadata

### Filesystem (`bridge:fs`)
- Read/write files
- Directory operations
- Path manipulation

### System (`bridge:sys`)
- System information (OS, architecture, hostname)
- Process information
- Resource usage

## Architecture

Clean Server consists of three main components:

1. **WASM Runtime**: Executes compiled Clean Language applications using Wasmtime
2. **HTTP Server**: Handles incoming HTTP requests and routes them to WASM handlers
3. **Host Bridge**: Provides secure system integration for WASM modules

```
┌─────────────────────────────────────────┐
│         Clean Server                    │
├─────────────────────────────────────────┤
│  HTTP Server (Axum)                     │
│    ↓                                    │
│  Router & Middleware                    │
│    ↓                                    │
│  WASM Runtime (Wasmtime)                │
│    ↓                                    │
│  Host Bridge                            │
│    ↓                                    │
│  System Resources                       │
│  (HTTP, DB, FS, Crypto, etc.)          │
└─────────────────────────────────────────┘
```

## Configuration

### Environment Variables

```bash
# Server configuration
CLEAN_SERVER_PORT=3000
CLEAN_SERVER_HOST=0.0.0.0
CLEAN_SERVER_LOG_LEVEL=info

# Database connection
DATABASE_URL=postgresql://user:pass@localhost/db

# Security
CLEAN_SERVER_CORS_ENABLED=true
CLEAN_SERVER_CORS_ORIGIN=https://example.com
CLEAN_SERVER_MAX_REQUEST_SIZE=10
```

### Production Deployment

#### Systemd Service (Linux)

Create `/etc/systemd/system/clean-server.service`:

```ini
[Unit]
Description=Clean Server
After=network.target

[Service]
Type=simple
User=www-data
WorkingDirectory=/var/www/app
ExecStart=/usr/local/bin/clean-server run app.wasm --port 3000
Restart=always
Environment="DATABASE_URL=postgresql://user:pass@localhost/db"

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
sudo systemctl enable clean-server
sudo systemctl start clean-server
```

#### Docker

```dockerfile
FROM debian:bookworm-slim

# Install dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy binary
COPY clean-server /usr/local/bin/clean-server
RUN chmod +x /usr/local/bin/clean-server

# Copy application
COPY app.wasm /app/app.wasm

WORKDIR /app
EXPOSE 3000

CMD ["clean-server", "run", "app.wasm", "--port", "3000", "--host", "0.0.0.0"]
```

Build and run:

```bash
docker build -t my-clean-app .
docker run -p 3000:3000 my-clean-app
```

## Performance

Clean Server is designed for high performance:

- **Fast startup**: < 100ms cold start
- **Low latency**: < 1ms p50 for simple endpoints
- **High throughput**: > 10,000 req/sec on modern hardware
- **Efficient memory**: < 50MB baseline memory usage

### Benchmarks

Benchmarked on MacBook Pro M1 Max (10 cores, 32GB RAM):

```
Endpoint Type          | p50    | p95    | p99    | Throughput
-----------------------|--------|--------|--------|------------
Static JSON            | 0.2ms  | 0.4ms  | 0.8ms  | 50,000 rps
Database Query         | 2.1ms  | 4.5ms  | 8.2ms  | 8,000 rps
HTTP Proxy             | 15.3ms | 28.1ms | 45.7ms | 1,200 rps
```

## Security

Clean Server implements defense-in-depth security:

- **WASM Sandboxing**: All application code runs in isolated WASM context
- **Host Bridge Allowlisting**: Explicit permissions for system access
- **SQL Injection Prevention**: Parameterized queries only
- **XSS Prevention**: Automatic HTML escaping
- **CSRF Protection**: Built-in token validation
- **Secure Cookies**: HTTP-only, SameSite, Secure flags
- **Rate Limiting**: Per-IP and per-endpoint limits
- **Input Validation**: Both compile-time and runtime checks

## Troubleshooting

### Common Issues

**Server won't start**
```bash
# Check if port is already in use
lsof -i :3000

# Try different port
clean-server run app.wasm --port 8080
```

**WASM module fails to load**
```bash
# Verify WASM file
file app.wasm

# Enable debug logging
clean-server run app.wasm --log-level debug
```

**Database connection errors**
```bash
# Test database connectivity
psql $DATABASE_URL -c "SELECT 1"

# Check environment variable
echo $DATABASE_URL
```

### Debug Mode

Enable verbose logging for troubleshooting:

```bash
RUST_LOG=clean_server=debug,host_bridge=debug clean-server run app.wasm
```

## Development

### Building from Source

```bash
# Clone repository
git clone https://github.com/Ivan-Pasco/clean-server.git
cd clean-server

# Build
cargo build --release

# Run tests
cargo test

# Run with example
cargo run --release -- run examples/hello.wasm
```

### Project Structure

```
clean-server/
├── src/
│   ├── main.rs           # CLI entry point
│   ├── lib.rs            # Library exports
│   ├── server.rs         # HTTP server
│   ├── router.rs         # Request routing
│   ├── wasm.rs           # WASM runtime
│   ├── bridge.rs         # Host Bridge integration
│   ├── memory.rs         # Memory management
│   └── error.rs          # Error types
├── host-bridge/
│   └── src/
│       ├── lib.rs        # Bridge exports
│       ├── http.rs       # HTTP client
│       ├── db.rs         # Database
│       ├── env.rs        # Environment
│       ├── time.rs       # Time operations
│       ├── crypto.rs     # Cryptography
│       ├── log.rs        # Logging
│       ├── fs.rs         # Filesystem
│       └── sys.rs        # System info
├── Cargo.toml
└── README.md
```

## Contributing

Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

## Links

- **Repository**: https://github.com/Ivan-Pasco/clean-server
- **Clean Language**: https://github.com/Ivan-Pasco/clean-language-compiler
- **Documentation**: https://docs.cleanlang.dev
- **Discord**: https://discord.gg/cleanlang

## Changelog

### v1.0.0 (2024-12-07)

- Initial release
- WebAssembly runtime using Wasmtime
- HTTP server with Axum
- Complete Host Bridge implementation
- Multi-platform support (Linux, macOS, Windows)
- Production-ready performance and security
