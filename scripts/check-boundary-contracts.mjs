#!/usr/bin/env node
/** Check and generate the first type-driven boundary contract. */

import { mkdirSync, readFileSync, writeFileSync } from "node:fs"
import { resolve } from "node:path"
import ts from "typescript"

const root = resolve(new URL("..", import.meta.url).pathname)

function unwrapExpression(expression) {
  if (ts.isAsExpression(expression) || ts.isTypeAssertionExpression(expression) || ts.isParenthesizedExpression(expression)) return unwrapExpression(expression.expression)
  return expression
}

function literalValue(expression, filePath) {
  const value = unwrapExpression(expression)
  if (ts.isStringLiteral(value) || ts.isNumericLiteral(value)) return value.text
  if (value.kind === ts.SyntaxKind.TrueKeyword) return true
  if (value.kind === ts.SyntaxKind.FalseKeyword) return false
  if (ts.isArrayLiteralExpression(value)) return value.elements.map(element => literalValue(element, filePath))
  if (ts.isObjectLiteralExpression(value)) {
    const result = {}
    for (const property of value.properties) {
      if (!ts.isPropertyAssignment(property)) throw new Error(`${filePath}: protocol objects must use property assignments`)
      const key = property.name.getText().replace(/^['"]|['"]$/g, "")
      result[key] = literalValue(property.initializer, filePath)
    }
    return result
  }
  if (value.kind === ts.SyntaxKind.NullKeyword) return null
  throw new Error(`${filePath}: unsupported protocol expression ${ts.SyntaxKind[value.kind]}`)
}

function exportedObject(filePath, exportName) {
  const sourceFile = ts.createSourceFile(filePath, readFileSync(filePath, "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
  let initializer
  const visit = node => {
    if (ts.isVariableDeclaration(node) && node.name.getText() === exportName) initializer = node.initializer
    ts.forEachChild(node, visit)
  }
  visit(sourceFile)
  if (!initializer) throw new Error(`${filePath}: exported protocol ${exportName} not found`)
  return literalValue(initializer, filePath)
}

function loadProtocolRegistry() {
  const registryPath = resolve(root, "contracts/protocols/registry.ts")
  const sourceFile = ts.createSourceFile(registryPath, readFileSync(registryPath, "utf8"), ts.ScriptTarget.Latest, true, ts.ScriptKind.TS)
  const imports = new Map()
  for (const statement of sourceFile.statements) {
    if (!ts.isImportDeclaration(statement) || !statement.importClause?.namedBindings || !ts.isNamedImports(statement.importClause.namedBindings)) continue
    const specifier = statement.moduleSpecifier.text.replace(/\.js$/, ".ts")
    for (const element of statement.importClause.namedBindings.elements) imports.set(element.name.text, resolve(root, "contracts/protocols", specifier))
  }
  let elements
  const visit = node => {
    if (ts.isVariableDeclaration(node) && node.name.getText() === "BOUNDARY_PROTOCOLS") {
      const initializer = unwrapExpression(node.initializer)
      if (initializer && ts.isArrayLiteralExpression(initializer)) elements = initializer.elements
    }
    ts.forEachChild(node, visit)
  }
  visit(sourceFile)
  if (!elements) throw new Error("contracts/protocols/registry.ts: BOUNDARY_PROTOCOLS must be an array")
  return elements.map(element => {
    const name = unwrapExpression(element).getText()
    const filePath = imports.get(name)
    if (!filePath) throw new Error(`registry.ts: protocol ${name} is not an imported protocol`)
    return exportedObject(filePath, name)
  })
}

const protocols = loadProtocolRegistry()

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
  if (!sourceFile) fail(`adapter file not found: ${filePath}`)
  let found
  const visit = node => {
    if (ts.isFunctionDeclaration(node) && node.name?.text === name) found = node
    ts.forEachChild(node, visit)
  }
  visit(sourceFile)
  if (!found) fail(`adapter function not found: ${name}`)
  return found
}

function propertyInfo(checker, type, context) {
  return checker.getPropertiesOfType(type).map(symbol => {
    const declaration = symbol.valueDeclaration ?? symbol.declarations?.[0]
    const optional = Boolean(declaration && ts.isPropertySignature(declaration) && declaration.questionToken)
    const symbolType = checker.getTypeOfSymbolAtLocation(symbol, context)
    return { name: symbol.getName(), optional, symbol, type: typeName(checker, symbolType) }
  })
}

function typeName(checker, type) {
  return checker.typeToString(type, undefined, ts.TypeFormatFlags.NoTruncation)
}

function inspectFields(checker, declaration, sourceType, targetType, protocol) {
  const sourceProperties = propertyInfo(checker, sourceType, declaration)
  const targetProperties = propertyInfo(checker, targetType, declaration)
  const sourceNames = new Set(sourceProperties.map(property => property.name))
  const targetNames = new Set(targetProperties.map(property => property.name))

  for (const field of protocol.fields.preserves ?? []) {
    if (!sourceNames.has(field)) fail(`preserved field "${field}" is absent from source type`)
    if (!targetNames.has(field)) fail(`preserved field "${field}" is absent from target type`)
  }
  for (const [sourceField, targetField] of Object.entries(protocol.fields.renames ?? {})) {
    if (!sourceNames.has(sourceField)) fail(`rename source "${sourceField}" is absent from source type`)
    if (!targetNames.has(targetField)) fail(`rename target "${targetField}" is absent from target type`)
    const sourceProperty = sourceProperties.find(property => property.name === sourceField)
    const targetProperty = targetProperties.find(property => property.name === targetField)
    const sourcePropertyType = checker.getTypeOfSymbolAtLocation(sourceProperty.symbol, declaration)
    const targetPropertyType = checker.getTypeOfSymbolAtLocation(targetProperty.symbol, declaration)
    // Renames may tighten optionality (e.g. `estimatedTokens?: number` → `estimated_tokens: number`
    // through an explicit default), so compare with undefined stripped from both sides.
    if (!checker.isTypeAssignableTo(checker.getNonNullableType(sourcePropertyType), checker.getNonNullableType(targetPropertyType))) {
      fail(`rename "${sourceField}" → "${targetField}" changes the field type: ${typeName(checker, sourcePropertyType)} is not assignable to ${typeName(checker, targetPropertyType)}`)
    }
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

  const requiredPreserves = inferredPreserves.filter(name => {
    const sourceProperty = sourceProperties.find(property => property.name === name)
    const targetProperty = targetProperties.find(property => property.name === name)
    return sourceProperty && targetProperty && !sourceProperty.optional && !targetProperty.optional
  })
  return { sourceProperties, targetProperties, inferredPreserves, requiredPreserves, inferredDrops }
}

function generateManifest(checker, declaration, signature, fields, protocol) {
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

function generateValidator(fields, protocol) {
  const validator = protocol.artifacts?.validator
  if (!validator) return undefined
  const forbidden = JSON.stringify(protocol.fields.forbidden, null, 2)
  const required = JSON.stringify(fields.requiredPreserves, null, 2)
  const allowed = JSON.stringify(fields.targetProperties.map(property => property.name), null, 2)
  const shapes = JSON.stringify(Object.fromEntries(fields.targetProperties.map(property => [property.name, runtimeShape(property.type)])), null, 2)
  // Progressive disclosure: when the crossing preserves lazy semantics, lazy content fields
  // must never appear in the projection — metadata crosses, content loads on activation.
  const lazyFields = protocol.lazy.lazySemantics === "preserve" ? (protocol.lazy.lazyFields ?? []) : []
  const lazy = JSON.stringify(lazyFields, null, 2)
  return `/**
 * Generated runtime validator for the ${validator.label} boundary.
 * DO NOT EDIT BY HAND - regenerate with: npm run contracts:check
 */

import type { ${validator.targetType} } from "${validator.targetImport}"

const FORBIDDEN_FIELDS = ${forbidden} as const
const LAZY_FIELDS = ${lazy} as const
const REQUIRED_PRESERVED_FIELDS = ${required} as const
const ALLOWED_TARGET_FIELDS = ${allowed} as const
const TARGET_FIELD_SHAPES = ${shapes} as const

function matchesShape(value: unknown, shape: string): boolean {
  if (shape === "string") return typeof value === "string"
  if (shape === "number") return typeof value === "number" && Number.isFinite(value)
  if (shape === "boolean") return typeof value === "boolean"
  if (shape === "array:string") return Array.isArray(value) && value.every(item => typeof item === "string")
  if (shape === "array:object") return Array.isArray(value) && value.every(item => typeof item === "object" && item !== null && !Array.isArray(item))
  if (shape === "array") return Array.isArray(value)
  if (shape === "object") return typeof value === "object" && value !== null && !Array.isArray(value)
  return true
}

export function ${validator.exportName}(
  result: unknown,
  options: { strict?: boolean } = {},
): asserts result is ${validator.targetType} {
  if (typeof result !== "object" || result === null) {
    throw new Error("${validator.label} validation failed: result must be an object")
  }

  const object = result as Record<string, unknown>
  for (const field of FORBIDDEN_FIELDS) {
    if (field in object) {
      throw new Error(
        \`${validator.label} validation failed: forbidden field "\${field}" leaked across boundary.\`,
      )
    }
  }
  for (const field of LAZY_FIELDS) {
    if (field in object) {
      throw new Error(
        \`${validator.label} validation failed: lazy field "\${field}" must not be materialized in the projection (progressive disclosure).\`,
      )
    }
  }
  for (const field of REQUIRED_PRESERVED_FIELDS) {
    if (!Object.prototype.hasOwnProperty.call(object, field)) {
      throw new Error(
        \`${validator.label} validation failed: required field "\${field}" is missing.\`,
      )
    }
  }
  for (const field of Object.keys(TARGET_FIELD_SHAPES)) {
    if (Object.prototype.hasOwnProperty.call(object, field) && !matchesShape(object[field], TARGET_FIELD_SHAPES[field as keyof typeof TARGET_FIELD_SHAPES])) {
      throw new Error(\`${validator.label} validation failed: field "\${field}" has an invalid type.\`)
    }
  }
  if (options.strict) {
    const allowed = new Set<string>(ALLOWED_TARGET_FIELDS)
    for (const key of Object.keys(object)) {
      if (!allowed.has(key)) {
        throw new Error(
          \`${validator.label} validation failed: unexpected field "\${key}" in result.\`,
        )
      }
    }
  }
}

export function ${validator.predicateName}(value: unknown): value is ${validator.targetType} {
  try {
    ${validator.exportName}(value)
    return true
  } catch {
    return false
  }
}
`
}

function runtimeShape(type) {
  const normalized = type.replace(/\s+/g, "").replace(/\|undefined/g, "")
  if (normalized === "string") return "string"
  if (normalized === "number") return "number"
  if (normalized === "boolean") return "boolean"
  if (normalized === "string[]" || normalized === "Array<string>") return "array:string"
  if (normalized.endsWith("[]")) return /Record<|object|\{/.test(normalized.slice(0, -2)) ? "array:object" : "array"
  if (/^(Array<|ReadonlyArray<)/.test(normalized)) {
    return /Record<|object|\{/.test(normalized) ? "array:object" : "array"
  }
  if (normalized.startsWith("Record<") || normalized === "object" || normalized.startsWith("{")) return "object"
  return "unknown"
}

function artifactPath(path) {
  return resolve(root, path)
}

function expandProtocol(protocol) {
  if (Array.isArray(protocol.adapters)) {
    return protocol.adapters.map(adapter => ({
      ...protocol,
      id: `${protocol.id}.${adapter.adapter.split(":").at(-1)}`,
      adapter: adapter.adapter,
      source: adapter.source,
      target: adapter.target,
      fields: adapter.fields ?? protocol.fields,
      artifacts: adapter.artifacts ?? protocol.artifacts,
      adapters: undefined,
    }))
  }
  if (protocol.adapter && protocol.source && protocol.target) return [protocol]
  fail(`protocol ${protocol.id} must declare adapter/source/target or adapters[]`)
}

function processProtocol(program, checker, protocol) {
  const [, adapterPath, adapterName] = protocol.adapter.match(/^([^:]+):(.+)$/) ?? []
  if (!adapterPath || !adapterName) fail(`invalid adapter reference: ${protocol.adapter}`)
  const adapterFilePath = resolve(root, "node/src", `${adapterPath}.ts`)
  const declaration = findAdapter(program, adapterFilePath, adapterName)
  const signature = checker.getSignatureFromDeclaration(declaration)
  if (!signature) fail(`could not resolve adapter signature for ${protocol.id}`)
  if (signature.parameters.length !== 1) fail(`${protocol.id}: expected one adapter parameter, got ${signature.parameters.length}`)

  const sourceType = checker.getTypeOfSymbolAtLocation(signature.parameters[0], declaration.parameters[0])
  const sourceName = typeName(checker, sourceType)
  const targetType = signature.getReturnType()
  const targetName = typeName(checker, targetType)
  if (sourceName !== protocol.source.type) fail(`${protocol.id}: adapter source type is ${sourceName}, expected ${protocol.source.type}`)
  if (targetName !== protocol.target.type) fail(`${protocol.id}: adapter target type is ${targetName}, expected ${protocol.target.type}`)

  const fields = inspectFields(checker, declaration, sourceType, targetType, protocol)
  const manifest = generateManifest(checker, declaration, signature, fields, protocol)
  const manifestPath = artifactPath(protocol.artifacts?.manifest ?? `contracts/manifests/${protocol.id.replace(/[^a-z0-9]+/gi, "-")}.json`)
  const validator = protocol.artifacts?.validator
  const validatorPath = validator ? artifactPath(validator.path) : undefined
  const manifestJson = JSON.stringify(manifest, null, 2) + "\n"
  const validatorSource = generateValidator(fields, protocol)
  // --verify compares generated content against disk without writing: the default
  // mode regenerates artifacts, so a trailing git diff cannot hide hand-edited drift.
  if (process.argv.includes("--verify")) {
    if (readFileSync(manifestPath, "utf8") !== manifestJson) {
      fail(`stale or hand-edited artifact: ${manifestPath} (run npm run contracts:check)`)
    }
    if (validatorPath && validatorSource !== undefined && readFileSync(validatorPath, "utf8") !== validatorSource) {
      fail(`stale or hand-edited artifact: ${validatorPath} (run npm run contracts:check)`)
    }
  } else {
    mkdirSync(resolve(manifestPath, ".."), { recursive: true })
    writeFileSync(manifestPath, manifestJson)
    if (validatorPath && validatorSource !== undefined) {
      mkdirSync(resolve(validatorPath, ".."), { recursive: true })
      writeFileSync(validatorPath, validatorSource)
    }
  }

  console.log(`  [${protocol.id}] ${sourceName} → ${targetName}`)
  console.log(`    Inferred preserves: ${fields.inferredPreserves.join(", ") || "(none)"}`)
  console.log(`    Inferred drops: ${fields.inferredDrops.join(", ") || "(none)"}`)
}

try {
  const adapters = protocols.flatMap(expandProtocol)
  console.log(`Checking ${adapters.length} registered boundary adapter${adapters.length === 1 ? "" : "s"} with the TypeScript compiler...`)
  const { program, checker } = createTypeChecker()
  for (const protocol of adapters) processProtocol(program, checker, protocol)
  if (process.argv.includes("--verify")) console.log("✅ Generated artifacts are in sync with the registry")
  else console.log("✅ Boundary contracts verified and artifacts generated")
} catch (error) {
  console.error("❌ Boundary contract check failed:")
  console.error(error instanceof Error ? error.stack : error)
  process.exitCode = 1
}
