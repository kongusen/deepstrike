#!/usr/bin/env node
import { readdirSync, readFileSync } from "node:fs"
import { join, resolve } from "node:path"

const root = resolve(new URL("..", import.meta.url).pathname)
const contracts = join(root, "contracts")
const requiredCrossingKeys = ["id", "source", "target", "crossing", "preserves", "drops", "forbidden", "kernel", "lossiness"]
const requiredBoundaryKeys = ["layer", "type", "authority"]
const sourceRoots = ["node/src", "wasm/src", "python/deepstrike", "rust/src"]
const sourceFiles = sourceRoots.flatMap(directory => {
  const files = []
  const visit = path => {
    for (const entry of readdirSync(join(root, path), { withFileTypes: true })) {
      const child = join(path, entry.name)
      if (entry.isDirectory()) visit(child)
      else if (/\.(ts|py|rs)$/.test(entry.name)) files.push({ path: child, text: readFileSync(join(root, child), "utf8") })
    }
  }
  visit(directory)
  return files
})
const source = sourceFiles.map(file => file.text).join("\n")

const normalizeType = value => String(value)
  .replace(/\s+/g, "")
  .replace(/readonly/g, "")
  .replace(/;$/, "")
const signatureFor = (text, symbol, method) => {
  const name = method || symbol.split(".").at(-1)
  const escaped = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
  const pattern = new RegExp(`(?:function\\s+${escaped}|${escaped})\\s*\\(([^)]*)\\)\\s*:\\s*([^\\{=>;]+)`)
  const match = text.match(pattern)
  if (!match) return undefined
  const parameters = match[1].trim() ? match[1].split(",").map(parameter => parameter.replace(/^\.\.\./, "").split("=")[0].trim().replace(/^\?/, "").replace(/^\w+\s*:\s*/, "")) : []
  return { parameters, returnType: match[2].split(/\r?\n/, 1)[0].split("//", 1)[0].split("/*", 1)[0].trim() }
}

const readJson = path => JSON.parse(readFileSync(join(contracts, path), "utf8"))
const vocabulary = readJson("vocabulary.json")
const portable = readJson("portable-run-session.json")
if (portable.version !== vocabulary.version || portable.authority !== "host-runtime") throw new Error("portable run/session contract is not pinned to the registry")
for (const required of ["PortableRunResult", "PortableSession"]) {
  if (!source.includes(required)) throw new Error(`portable contract type missing from SDK projections: ${required}`)
}
const authority = readJson("authority.json")
const authorityNames = new Set(Object.values(authority))
authorityNames.add("none")
const verbNames = new Set(Array.isArray(vocabulary.verbs) ? vocabulary.verbs : Object.keys(vocabulary.verbs))
const projectedVocabulary = JSON.parse(readFileSync(join(root, "tests/fixtures/runtime-language/vocabulary.json"), "utf8"))
for (const [registryLayer, projectedLayer] of [["public", "public"], ["runtime", "host"], ["kernel", "kernel"], ["provider", "provider"]]) {
  if (JSON.stringify(vocabulary.layers[registryLayer]) !== JSON.stringify(projectedVocabulary[projectedLayer])) throw new Error(`vocabulary projection drift: ${registryLayer}`)
}
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
  for (const side of ["source", "target"]) if (!authorityNames.has(contract[side].authority)) throw new Error(`${file}: unknown authority ${contract[side].authority}`)
  if (!verbNames.has(contract.crossing.verb)) throw new Error(`${file}: unknown crossing verb ${contract.crossing.verb}`)
  const fn = contract.crossing.function.split(".").at(-1)
  if (!contract.implementation?.file || !contract.implementation?.symbol) throw new Error(`${file}: implementation.file and implementation.symbol are required`)
  const implementation = sourceFiles.find(candidate => candidate.path === contract.implementation.file)
  if (!implementation) throw new Error(`${file}: implementation file not found: ${contract.implementation.file}`)
  const symbol = contract.implementation.symbol.split(".").at(-1)
  if (!implementation.text.includes(symbol)) throw new Error(`${file}: implementation symbol not found: ${contract.implementation.symbol}`)
  if (contract.implementation.parameterTypes || contract.implementation.returnType) {
    const signature = signatureFor(implementation.text, contract.implementation.symbol, contract.implementation.method)
    if (!signature) throw new Error(`${file}: implementation signature not found: ${contract.implementation.symbol}`)
    const expectedParameters = contract.implementation.parameterTypes ?? []
    if (expectedParameters.length !== signature.parameters.length || expectedParameters.some((expected, index) => normalizeType(expected) !== normalizeType(signature.parameters[index]))) {
      throw new Error(`${file}: implementation parameter signature drift for ${contract.implementation.symbol}: expected ${expectedParameters.join(", ")}, got ${signature.parameters.join(", ")}`)
    }
    if (contract.implementation.returnType && normalizeType(contract.implementation.returnType) !== normalizeType(signature.returnType)) {
      throw new Error(`${file}: implementation return signature drift for ${contract.implementation.symbol}: expected ${contract.implementation.returnType}, got ${signature.returnType}`)
    }
  }
  if (!source.includes(fn)) throw new Error(`${file}: named crossing function not found: ${contract.crossing.function}`)
  if (contract.kernelProjection?.implementation) {
    const projection = contract.kernelProjection.implementation
    const projectionFile = sourceFiles.find(candidate => candidate.path === projection.file)
    if (!projectionFile || !projectionFile.text.includes(projection.symbol.split(".").at(-1))) throw new Error(`${file}: kernel projection implementation not found`)
  }
  if (!Array.isArray(contract.preserves) || !Array.isArray(contract.drops) || !Array.isArray(contract.forbidden)) throw new Error(`${file}: semantic lists must be arrays`)
}
const forbidden = readJson("forbidden-crossings.json")
for (const [crossing, policy] of Object.entries(forbidden)) if (!String(policy).startsWith("forbidden")) throw new Error(`${crossing}: invalid forbidden policy`)
const identities = readJson("identity.json")
for (const [name, rule] of Object.entries(identities)) {
  if (rule.remint !== false) throw new Error(`${name}: crossing identity remint must be false`)
  if (!Array.isArray(rule.allowedMintSites) || rule.allowedMintSites.length === 0) throw new Error(`${name}: allowedMintSites is required`)
  if (rule.allowedMintSites.some(site => !/[#:]/.test(site) || /^(public_or_host|host|kernel|runtime_content_addressed)$/.test(site))) {
    throw new Error(`${name}: allowedMintSites must name concrete module/symbol sites`)
  }
  for (const site of rule.allowedMintSites) {
    const file = site.split("#")[0].replace(/^node:/, "node/").replace(/^wasm:/, "wasm/").replace(/^rust:/, "rust/")
    if (file.includes("/") && !sourceFiles.some(candidate => candidate.path === file)) {
      if (!site.startsWith("rust:deepstrike_core")) throw new Error(`${name}: mint site file not found: ${site}`)
    }
  }
}
const hostIdentityMint = sourceFiles.filter(file => !file.path.startsWith("rust/src/") && !file.path.startsWith("wasm/src/runtime/canonical-kernel-step.ts"))
if (hostIdentityMint.some(file => /(?:effectId|effect_id)\s*[:=][^\n]*(?:randomUUID|uuid4|new_v4)/.test(file.text))) {
  throw new Error("EffectId must be minted by the kernel projection only")
}
const publicSources = sourceFiles.filter(file => ["node/src/agent.ts", "node/src/skill.ts", "node/src/evals/public.ts"].includes(file.path))
if (publicSources.some(file => /runtime\/kernel|KernelJournal|ProviderAttempt|RuntimeRunner/.test(file.text))) throw new Error("public layer imports kernel/runtime authority directly")
const providerSources = sourceFiles.filter(file => file.path.startsWith("node/src/providers/"))
if (providerSources.some(file => /runtime\/kernel|KernelJournal|EffectId|OperationId/.test(file.text))) throw new Error("provider membrane imports kernel authority directly")
if (authority["AgentDefinition"] !== "public-agent" || authority["Operation"] !== "kernel") throw new Error("authority registry does not pin core owners")
console.log(`Semantic contract registry passed (${crossingFiles.length} crossings, ${Object.keys(forbidden).length} forbidden rules)`)
