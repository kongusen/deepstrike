#!/usr/bin/env node
import { readdirSync, readFileSync } from "node:fs"
import { join, resolve } from "node:path"

const root = resolve(new URL("..", import.meta.url).pathname)
const contracts = join(root, "contracts")
const requiredCrossingKeys = ["id", "source", "target", "crossing", "preserves", "drops", "forbidden", "kernel", "lossiness"]
const requiredBoundaryKeys = ["layer", "type", "authority"]
const sourceRoots = ["node/src", "wasm/src", "python/deepstrike", "rust/src"]
const source = sourceRoots.flatMap(directory => {
  const files = []
  const visit = path => {
    for (const entry of readdirSync(join(root, path), { withFileTypes: true })) {
      const child = join(path, entry.name)
      if (entry.isDirectory()) visit(child)
      else if (/\.(ts|py|rs)$/.test(entry.name)) files.push(readFileSync(join(root, child), "utf8"))
    }
  }
  visit(directory)
  return files
}).join("\n")

const readJson = path => JSON.parse(readFileSync(join(contracts, path), "utf8"))
const vocabulary = readJson("vocabulary.json")
if (vocabulary.version !== "0.2.74") throw new Error("semantic contract registry version drift")
for (const layer of ["public", "runtime", "kernel", "provider"]) {
  if (!Array.isArray(vocabulary.layers[layer]) || vocabulary.layers[layer].length === 0) throw new Error(`missing vocabulary layer: ${layer}`)
}
const crossingFiles = readdirSync(join(contracts, "crossings")).filter(name => name.endsWith(".json"))
if (crossingFiles.length === 0) throw new Error("semantic contract registry has no crossings")
for (const file of crossingFiles) {
  const contract = readJson(join("crossings", file))
  for (const key of requiredCrossingKeys) if (!(key in contract)) throw new Error(`${file}: missing ${key}`)
  for (const side of ["source", "target"]) for (const key of requiredBoundaryKeys) if (!(key in contract[side])) throw new Error(`${file}: ${side} missing ${key}`)
  if (!vocabulary.verbs.includes(contract.crossing.verb)) throw new Error(`${file}: unknown crossing verb ${contract.crossing.verb}`)
  const fn = contract.crossing.function.split(".").at(-1)
  if (!source.includes(fn)) throw new Error(`${file}: named crossing function not found: ${contract.crossing.function}`)
  if (!Array.isArray(contract.preserves) || !Array.isArray(contract.drops) || !Array.isArray(contract.forbidden)) throw new Error(`${file}: semantic lists must be arrays`)
}
const forbidden = readJson("forbidden-crossings.json")
for (const [crossing, policy] of Object.entries(forbidden)) if (!String(policy).startsWith("forbidden")) throw new Error(`${crossing}: invalid forbidden policy`)
const identities = readJson("identity.json")
for (const [name, rule] of Object.entries(identities)) if (rule.remint !== false) throw new Error(`${name}: crossing identity remint must be false`)
console.log(`Semantic contract registry passed (${crossingFiles.length} crossings, ${Object.keys(forbidden).length} forbidden rules)`)
