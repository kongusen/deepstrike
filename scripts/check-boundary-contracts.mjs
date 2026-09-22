#!/usr/bin/env node
/**
 * Type-driven boundary contract checker (simplified).
 *
 * Validates that adapter implementations match their protocol declarations
 * by parsing TypeScript source code directly.
 *
 * Task 3: Build type inspection for the skill host-to-kernel adapter.
 */

import { readFileSync } from "node:fs"
import { resolve } from "node:path"

const root = resolve(new URL("..", import.meta.url).pathname)

/**
 * Extract function signature from TypeScript source.
 */
function extractFunctionSignature(sourceText, functionName) {
  // Match: export function name(param: Type): ReturnType
  // or: export const name = (param: Type): ReturnType =>
  const patterns = [
    new RegExp(`export\\s+function\\s+${functionName}\\s*\\(([^)]*)\\)\\s*:\\s*([^{;]+)`, 's'),
    new RegExp(`export\\s+const\\s+${functionName}\\s*=\\s*\\(([^)]*)\\)\\s*:\\s*([^=>{]+)`, 's'),
  ]

  for (const pattern of patterns) {
    const match = sourceText.match(pattern)
    if (match) {
      const paramsStr = match[1].trim()
      const returnType = match[2].trim()

      // Parse parameters
      const parameters = []
      if (paramsStr) {
        // Simple parameter parsing (doesn't handle complex nested types)
        const paramParts = paramsStr.split(',')
        for (const part of paramParts) {
          const colonIndex = part.indexOf(':')
          if (colonIndex > 0) {
            const name = part.slice(0, colonIndex).trim()
            const type = part.slice(colonIndex + 1).trim()
            parameters.push({ name, type })
          }
        }
      }

      return { parameters, returnType }
    }
  }

  return null
}

/**
 * Extract interface properties from TypeScript source.
 */
function extractInterfaceProperties(sourceText, interfaceName) {
  // Match: export interface Name { ... }
  const pattern = new RegExp(`export\\s+interface\\s+${interfaceName}\\s*\\{([^}]+)\\}`, 's')
  const match = sourceText.match(pattern)

  if (!match) return null

  const body = match[1]
  const properties = []

  // Match property lines: name?: type or name: type
  const propPattern = /^\s*(\w+)(\??):\s*([^\n;]+)/gm
  let propMatch

  while ((propMatch = propPattern.exec(body)) !== null) {
    const name = propMatch[1]
    const optional = propMatch[2] === '?'
    properties.push({ name, optional })
  }

  return properties
}

/**
 * Check the skill host-to-kernel projection adapter.
 */
async function checkSkillHostToKernelAdapter() {
  console.log("Checking skill host-to-kernel adapter...")

  // Read source files
  const kernelStepPath = resolve(root, "node/src/runtime/kernel-step.ts")
  const loaderPath = resolve(root, "node/src/skills/loader.ts")

  const kernelStepSource = readFileSync(kernelStepPath, "utf8")
  const loaderSource = readFileSync(loaderPath, "utf8")

  // Extract adapter signature
  const signature = extractFunctionSignature(kernelStepSource, "skillMetadataToKernel")
  if (!signature) {
    throw new Error("skillMetadataToKernel function signature not found")
  }

  console.log(`  Adapter signature: (${signature.parameters.map(p => `${p.name}: ${p.type}`).join(", ")}) => ${signature.returnType}`)

  // Verify parameter type
  if (signature.parameters.length !== 1) {
    throw new Error(`Expected 1 parameter, got ${signature.parameters.length}`)
  }
  if (signature.parameters[0].type !== "SkillMetadata") {
    throw new Error(`Expected parameter type SkillMetadata, got ${signature.parameters[0].type}`)
  }

  // Verify return type
  if (signature.returnType !== "KernelSkillMetadata") {
    throw new Error(`Expected return type KernelSkillMetadata, got ${signature.returnType}`)
  }

  // Extract type properties
  const sourceProps = extractInterfaceProperties(loaderSource, "SkillMetadata")
  if (!sourceProps) {
    throw new Error("SkillMetadata interface not found")
  }

  const targetProps = extractInterfaceProperties(kernelStepSource, "KernelSkillMetadata")
  if (!targetProps) {
    throw new Error("KernelSkillMetadata interface not found")
  }

  console.log(`  Source type (SkillMetadata): ${sourceProps.length} properties`)
  console.log(`    ${sourceProps.map(p => `${p.name}${p.optional ? '?' : ''}`).join(", ")}`)

  console.log(`  Target type (KernelSkillMetadata): ${targetProps.length} properties`)
  console.log(`    ${targetProps.map(p => `${p.name}${p.optional ? '?' : ''}`).join(", ")}`)

  // Verify required fields
  const requiredSourceFields = sourceProps.filter(p => !p.optional).map(p => p.name)
  console.log(`  Required source fields: ${requiredSourceFields.join(", ")}`)

  // Load the protocol declaration
  const protocolModule = await import("../contracts/protocols/skill-host-to-kernel.js")
  const protocol = protocolModule.SKILL_HOST_TO_KERNEL_PROTOCOL

  // Verify preserves are present in both source and target
  for (const field of protocol.fields.preserves || []) {
    const inSource = sourceProps.some(p => p.name === field)
    const inTarget = targetProps.some(p => p.name === field)

    if (!inSource) {
      throw new Error(`Protocol declares preserved field "${field}" but it's not in SkillMetadata`)
    }
    if (!inTarget) {
      throw new Error(`Protocol declares preserved field "${field}" but it's not in KernelSkillMetadata`)
    }
  }

  // Verify renames exist in source and target (with correct names)
  for (const [sourceField, targetField] of Object.entries(protocol.fields.renames || {})) {
    const inSource = sourceProps.some(p => p.name === sourceField)
    const inTarget = targetProps.some(p => p.name === targetField)

    if (!inSource) {
      throw new Error(`Protocol declares rename from "${sourceField}" but it's not in SkillMetadata`)
    }
    if (!inTarget) {
      throw new Error(`Protocol declares rename to "${targetField}" but it's not in KernelSkillMetadata`)
    }
  }

  console.log("✅ Skill host-to-kernel adapter verified")

  return {
    adapter: "skillMetadataToKernel",
    signature,
    sourceProps,
    targetProps,
    protocol,
  }
}

/**
 * Generate crossing manifest from verification results.
 */
function generateManifest(result) {
  const { adapter, signature, sourceProps, targetProps, protocol } = result

  // Infer exact preserves (fields with same name in both types)
  const inferredPreserves = sourceProps
    .filter(sp => targetProps.some(tp => tp.name === sp.name))
    .map(p => p.name)

  // Infer drops (fields in source but not in target, excluding renamed fields)
  const renamedSourceFields = Object.keys(protocol.fields.renames || {})
  const inferredDrops = sourceProps
    .filter(sp => {
      const inTarget = targetProps.some(tp => tp.name === sp.name)
      const isRenamed = renamedSourceFields.includes(sp.name)
      return !inTarget && !isRenamed
    })
    .map(p => p.name)

  const manifest = {
    id: "skill.host-to-kernel",
    version: "0.2.74",
    protocol: {
      family: protocol.family,
      direction: protocol.direction,
      source: protocol.source,
      target: protocol.target,
      adapter: protocol.adapter,
      lossiness: protocol.lossiness,
    },
    fields: {
      preserves: {
        declared: protocol.fields.preserves || [],
        inferred: inferredPreserves,
      },
      renames: protocol.fields.renames || {},
      drops: {
        declared: protocol.fields.drops || [],
        inferred: inferredDrops,
      },
      forbidden: protocol.fields.forbidden,
    },
    lazy: protocol.lazy,
    adapterSignature: {
      parameters: signature.parameters,
      returnType: signature.returnType,
    },
    generatedAt: new Date().toISOString(),
  }

  return manifest
}

// Run the checker
try {
  const result = await checkSkillHostToKernelAdapter()

  // Generate and save manifest
  const manifest = generateManifest(result)
  const manifestPath = resolve(root, "contracts/manifests/skill-host-to-kernel.json")
  const { mkdirSync, writeFileSync } = await import("node:fs")
  mkdirSync(resolve(root, "contracts/manifests"), { recursive: true })
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + "\n")

  console.log(`\n📄 Generated manifest: contracts/manifests/skill-host-to-kernel.json`)
  console.log(`   Inferred preserves: ${manifest.fields.preserves.inferred.join(", ")}`)
  console.log(`   Inferred drops: ${manifest.fields.drops.inferred.join(", ") || "(none)"}`)
  console.log("\n✅ All boundary contract checks passed")
  process.exit(0)
} catch (error) {
  console.error("\n❌ Boundary contract check failed:")
  console.error(error.message)
  process.exit(1)
}
