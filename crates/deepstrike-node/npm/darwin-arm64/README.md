# `@deepstrike/core-darwin-arm64`

Platform-specific native addon for [`@deepstrike/core`](https://www.npmjs.com/package/@deepstrike/core).

- **Platform:** macOS ARM64 (Apple Silicon)
- **Target triple:** `aarch64-apple-darwin`

## Do not install directly

This package is an internal binary dependency. Install [`@deepstrike/sdk`](https://www.npmjs.com/package/@deepstrike/sdk) instead — the correct platform package is selected and installed automatically via `optionalDependencies`.

```bash
npm install @deepstrike/sdk
```

## How it works

`@deepstrike/core` loads this package at runtime when running on macOS with Apple Silicon. The `.node` file is a compiled Rust extension built with [napi-rs](https://napi.rs) that exposes the DeepStrike kernel (loop control, context compression, governance, signal routing) to Node.js.

## License

Apache-2.0 OR MIT
