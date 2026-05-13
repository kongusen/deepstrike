# `@deepstrike/core-linux-arm64-gnu`

Platform-specific native addon for [`@deepstrike/core`](https://www.npmjs.com/package/@deepstrike/core).

- **Platform:** Linux ARM64 (glibc)
- **Target triple:** `aarch64-unknown-linux-gnu`

## Do not install directly

This package is an internal binary dependency. Install [`@deepstrike/sdk`](https://www.npmjs.com/package/@deepstrike/sdk) instead — the correct platform package is selected and installed automatically via `optionalDependencies`.

```bash
npm install @deepstrike/sdk
```

## How it works

`@deepstrike/core` loads this package at runtime when running on Linux ARM64 with glibc (e.g. AWS Graviton, Raspberry Pi OS 64-bit). The `.node` file is a compiled Rust extension built with [napi-rs](https://napi.rs) that exposes the DeepStrike kernel (loop control, context compression, governance, signal routing) to Node.js.

## License

Apache-2.0 OR MIT
