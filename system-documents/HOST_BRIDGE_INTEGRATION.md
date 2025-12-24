# Host Bridge Integration

**This documentation has moved to the parent platform-architecture folder.**

See: [/platform-architecture/README.md](../../platform-architecture/README.md)

## Quick Links

- [Host Bridge Specification](../../platform-architecture/HOST_BRIDGE.md) - All portable host functions
- [Memory Model](../../platform-architecture/MEMORY_MODEL.md) - WASM memory layout
- [Server Extensions](../../platform-architecture/SERVER_EXTENSIONS.md) - HTTP server functions
- [Implementing a New Host](../../platform-architecture/IMPLEMENTING_HOST.md) - New runtime guide

## Summary

The host-bridge library provides portable WASM host functions that work across all Clean Language runtimes:
- Console I/O (14 functions)
- Math (30+ functions)
- String operations (25+ functions)
- Memory runtime (5 functions)
- Database (5 functions)
- File I/O (5 functions)
- HTTP client (20+ functions)
- Crypto (4 functions)

Server-specific functions (HTTP routing, request context, auth) remain in clean-server.
