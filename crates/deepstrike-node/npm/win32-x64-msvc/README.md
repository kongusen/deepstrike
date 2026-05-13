# `@deepstrike/core-win32-x64-msvc`

Platform-specific native addon for [`@deepstrike/core`](https://www.npmjs.com/package/@deepstrike/core).

- **Platform:** Windows x64 (MSVC)
- **Target triple:** `x86_64-pc-windows-msvc`

## Do not install directly

This package is an internal binary dependency. Install [`@deepstrike/sdk`](https://www.npmjs.com/package/@deepstrike/sdk) instead — the correct platform package is selected and installed automatically via `optionalDependencies`.

```bash
npm install @deepstrike/sdk
```

## How it works

`@deepstrike/core` loads this package at runtime when running on Windows x64. The `.node` file is a compiled Rust extension built with [napi-rs](https://napi.rs) that exposes the DeepStrike kernel (loop control, context compression, governance, signal routing) to Node.js.

## License

Apache-2.0 OR MIT
