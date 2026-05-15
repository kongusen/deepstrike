# `@deepstrike/core`

Native DeepStrike kernel loader for Node.js.

This package contains the JavaScript loader and TypeScript definitions. The native
`.node` binary is shipped in a platform-specific optional dependency such as
`@deepstrike/core-linux-x64-gnu` or `@deepstrike/core-darwin-arm64`.

Install `@deepstrike/sdk` for normal use:

```bash
npm install @deepstrike/sdk
```

The install does not run a postinstall downloader. npm selects the matching
platform package through `optionalDependencies`.
