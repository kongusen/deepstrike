'use strict'

const { existsSync, readFileSync } = require('fs')
const { join } = require('path')

const { platform, arch } = process

const supportedTriples = [
  'darwin-arm64',
  'darwin-x64',
  'linux-arm64-gnu',
  'linux-arm64-musl',
  'linux-x64-gnu',
  'linux-x64-musl',
  'win32-x64-msvc',
]

function readIfExists(path) {
  try {
    if (existsSync(path)) return readFileSync(path, 'utf8')
  } catch {
    return ''
  }
  return ''
}

function isMusl() {
  if (platform !== 'linux') return false

  const report = typeof process.report?.getReport === 'function'
    ? process.report.getReport()
    : null
  if (report?.header?.glibcVersionRuntime) return false

  const ldd = readIfExists('/usr/bin/ldd') || readIfExists('/bin/ldd')
  if (ldd.includes('musl')) return true
  if (ldd.includes('GNU libc') || ldd.includes('glibc')) return false

  return false
}

function packageTriple() {
  if (platform === 'darwin') {
    if (arch === 'arm64' || arch === 'x64') return `darwin-${arch}`
  }

  if (platform === 'win32') {
    if (arch === 'x64') return 'win32-x64-msvc'
  }

  if (platform === 'linux') {
    if (arch === 'arm64' || arch === 'x64') {
      return `linux-${arch}-${isMusl() ? 'musl' : 'gnu'}`
    }
  }

  return null
}

function requireLocal(triple) {
  const candidates = [
    `deepstrike-core.${triple}.node`,
    'deepstrike-core.node',
  ]

  for (const candidate of candidates) {
    const path = join(__dirname, candidate)
    if (existsSync(path)) return require(path)
  }

  return null
}

function loadNativeBinding() {
  const triple = packageTriple()
  if (!triple || !supportedTriples.includes(triple)) {
    throw new Error(
      `Unsupported DeepStrike native platform: ${platform}/${arch}. ` +
      `Supported targets: ${supportedTriples.join(', ')}.`,
    )
  }

  const local = requireLocal(triple)
  if (local) return local

  const packageName = `@deepstrike/core-${triple}`
  try {
    return require(packageName)
  } catch (error) {
    const cause = error && typeof error === 'object' && 'message' in error
      ? `\nCause: ${error.message}`
      : ''
    throw new Error(
      `Failed to load ${packageName}. ` +
      `Install @deepstrike/sdk or @deepstrike/core normally so npm can install ` +
      `the matching optional native package for ${platform}/${arch}.${cause}`,
    )
  }
}

module.exports = loadNativeBinding()
