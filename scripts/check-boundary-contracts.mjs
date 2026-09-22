#!/usr/bin/env node
/** Check and generate the first type-driven boundary contract. */

import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { resolve } from "node:path"
import ts from "typescript"

const root = resolve(new URL("..", import.meta.url).pathname)
const registryModule = await import("../contracts/protocols/registry.js")
const protocols = registryModule.BOUNDARY_PROTOCOLS
if (!Array.isArray(protocols) || protocols.length === 0) throw new Error("boundary protocol registry is empty")
const protocol = protocols.find(candidate => candidate.id === "skill.host-to-kernel")
if (!protocol) throw new Error("skill.host-to-kernel protocol is not registered")
const [, adapterPath, adapterName] = protocol.adapter.match(/^([^:]+):(.+)$/) ?? []
if (!adapterPath || !adapterName) throw new Error(`invalid adapter reference: ${protocol.adapter}`)
const kernelStepPath = resolve(root, "node/src", `${adapterPath}.ts`)

function fail(message) { throw new Error(message) }

function createTypeChecker() {
  const configPath = resolve(root, "node/tsconfig.json")
  const config = ts.readConfigFile(configPath, file => readFileSync(file, "utf8"))
  if (config.error) fail(ts.flattenDiagnosticMessageText(config.error.messageText, "\n"))
  const parsed = ts.parseJsonConfigFileContent(config.config, ts.sys, resolve(root, "node"))
  const program = ts.createProgram(parsed.fileNames, parsed.options)
  const diagnostics = ts.getPreEmitDiagnostics(program)
  if (diagnostics.length) {
    fail(ts.formatDiagnosticsWithColorAndContext(diagnostics, {
      getCanonicalFileName: fileName => fileName,
      getCurrentDirectory: () => root,
      getNewLine: () => "\n",
    }))
  }
  return { program, checker: program.getTypeChecker() }
}

function findAdapter(program, filePath, name) {
  const sourceFile = program.getSourceFile(filePath)
  if (!sourceFile) fail(`adapter file not found: ${kernelStepPath}`)
  let found
  const visit = node => {
    if (ts.isFunctionDeclaration(node) && node.name?.text === name) found = node
    ts.forEachChild(node, visit)
  }
  visit(sourceFile)
  if (!found) fail(`adapter function not found: ${name}`)
  return found
}

function propertyInfo(checker, type) {
  return checker.getPropertiesOfType(type).map(symbol => {
    const declaration = symbol.valueDeclaration ?? symbol.declarations?.[0]
    const optional = Boolean(declaration && ts.isPropertySignature(declaration) && declaration.questionToken)
    return { name: symbol.getName(), optional, symbol }
  })
}

function typeName(checker, type) {
  return checker.typeToString(type, undefined, ts.TypeFormatFlags.NoTruncation)
}

function inspectFields(checker, declaration, sourceType, targetType) {
  const sourceProperties = propertyInfo(checker, sourceType)
  const targetProperties = propertyInfo(checker, targetType)
  const sourceNames = new Set(sourceProperties.map(property => property.name))
  const targetNames = new Set(targetProperties.map(property => property.name))

  for (const field of protocol.fields.preserves ?? []) {
    if (!sourceNames.has(field)) fail(`preserved field "${field}" is absent from source type`)
    if (!targetNames.has(field)) fail(`preserved field "${field}" is absent from target type`)
  }
  for (const [sourceField, targetField] of Object.entries(protocol.fields.renames ?? {})) {
    if (!sourceNames.has(sourceField)) fail(`rename source "${sourceField}" is absent from source type`)
    if (!targetNames.has(targetField)) fail(`rename target "${targetField}" is absent from target type`)
  }
  for (const field of protocol.fields.derived ?? []) {
    if (!targetNames.has(field)) fail(`derived field "${field}" is absent from target type`)
  }

  const inferredPreserves = sourceProperties.filter(sourceProperty => {
    const targetProperty = targetProperties.find(candidate => candidate.name === sourceProperty.name)
    if (!targetProperty) return false
    const sourcePropertyType = checker.getTypeOfSymbolAtLocation(sourceProperty.symbol, declaration)
    const targetPropertyType = checker.getTypeOfSymbolAtLocation(targetProperty.symbol, declaration)
    return checker.isTypeAssignableTo(sourcePropertyType, targetPropertyType)
  }).map(property => property.name)

  const renamedSourceFields = new Set(Object.keys(protocol.fields.renames ?? {}))
  const inferredDrops = sourceProperties
    .filter(property => !targetNames.has(property.name) && !renamedSourceFields.has(property.name))
    .map(property => property.name)

  return { sourceProperties, targetProperties, inferredPreserves, inferredDrops }
}

function generateManifest(checker, declaration, signature, fields) {
  const sourceType = checker.getTypeOfSymbolAtLocation(signature.parameters[0], declaration.parameters[0])
  return {
    id: protocol.id,
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
      preserves: { declared: protocol.fields.preserves ?? [], inferred: fields.inferredPreserves },
      renames: protocol.fields.renames ?? {},
      drops: { declared: protocol.fields.drops ?? [], inferred: fields.inferredDrops },
      derived: protocol.fields.derived ?? [],
      forbidden: protocol.fields.forbidden,
    },
    lazy: protocol.lazy,
    adapterSignature: {
      parameters: signature.parameters.map((parameter, index) => ({
        name: declaration.parameters[index].name.getText(),
        type: typeName(checker, checker.getTypeOfSymbolAtLocation(parameter, declaration.parameters[index])),
      })),
      returnType: typeName(checker, signature.getReturnType()),
    },
  }
}

function generateValidator(fields) {
  const forbidden = JSON.stringify(protocol.fields.forbidden, null, 2)
  const required = JSON.stringify(protocol.fields.preserves ?? [], null, 2)
  const allowed = JSON.stringify(fields.targetProperties.map(property => property.name), null, 2)
  return `/**
 * Generated runtime validator for the skill host-to-kernel boundary.
 * DO NOT EDIT BY HAND - regenerate with: npm run contracts:check
 */

import type { KernelSkillMetadata } from "../kernel-step.js"

const FORBIDDEN_FIELDS = ${forbidden} as const
const REQUIRED_PRESERVED_FIELDS = ${required} as const
const ALLOWED_TARGET_FIELDS = ${allowed} as const

export function validateSkillKernelProjection(
  result: unknown,
  options: { strict?: boolean } = {},
): asserts result is KernelSkillMetadata {
  if (typeof result !== "object" || result === null) {
    throw new Error("Skill kernel projection validation failed: result must be an object")
  }

  const object = result as Record<string, unknown>
  for (const field of FORBIDDEN_FIELDS) {
    if (field in object) {
      throw new Error(
        \`Skill kernel projection validation failed: forbidden field "\${field}" leaked across boundary.\`,
      )
    }
  }
  for (const field of REQUIRED_PRESERVED_FIELDS) {
    if (!Object.prototype.hasOwnProperty.call(object, field)) {
      throw new Error(
        \`Skill kernel projection validation failed: required field "\${field}" is missing.\`,
      )
    }
  }
  if (options.strict) {
    const allowed = new Set<string>(ALLOWED_TARGET_FIELDS)
    for (const key of Object.keys(object)) {
      if (!allowed.has(key)) {
        throw new Error(
          \`Skill kernel projection validation failed: unexpected field "\${key}" in result.\`,
        )
      }
    }
  }
}

export function isKernelSkillMetadata(value: unknown): value is KernelSkillMetadata {
  try {
    validateSkillKernelProjection(value)
    return true
  } catch {
    return false
  }
}
`
}

try {
  console.log("Checking skill host-to-kernel adapter with the TypeScript compiler...")
  const { program, checker } = createTypeChecker()
  const declaration = findAdapter(program, kernelStepPath, adapterName)
  const signature = checker.getSignatureFromDeclaration(declaration)
  if (!signature) fail("could not resolve adapter signature")
  if (signature.parameters.length !== 1) fail(`expected one adapter parameter, got ${signature.parameters.length}`)

  const sourceType = checker.getTypeOfSymbolAtLocation(signature.parameters[0], declaration.parameters[0])
  const sourceName = typeName(checker, sourceType)
  const targetType = signature.getReturnType()
  const targetName = typeName(checker, targetType)
  if (sourceName !== protocol.source.type) fail(`adapter source type is ${sourceName}, expected ${protocol.source.type}`)
  if (targetName !== protocol.target.type) fail(`adapter target type is ${targetName}, expected ${protocol.target.type}`)

  const fields = inspectFields(checker, declaration, sourceType, targetType)
  const manifest = generateManifest(checker, declaration, signature, fields)
  const manifestPath = resolve(root, "contracts/manifests/skill-host-to-kernel.json")
  mkdirSync(resolve(root, "contracts/manifests"), { recursive: true })
  writeFileSync(manifestPath, JSON.stringify(manifest, null, 2) + "\n")
  writeFileSync(
    resolve(root, "node/src/runtime/validators/skill-kernel-projection.ts"),
    generateValidator(fields),
  )

  console.log(`  Adapter: ${sourceName} → ${targetName}`)
  console.log(`  Inferred preserves: ${fields.inferredPreserves.join(", ") || "(none)"}`)
  console.log(`  Inferred drops: ${fields.inferredDrops.join(", ") || "(none)"}`)
  console.log("✅ Boundary contract verified and manifest generated")
} catch (error) {
  console.error("❌ Boundary contract check failed:")
  console.error(error instanceof Error ? error.stack : error)
  process.exitCode = 1
}
