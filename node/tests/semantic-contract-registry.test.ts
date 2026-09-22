import { readFileSync, readdirSync } from "node:fs"
import { join } from "node:path"

const root = join(process.cwd(), "..")
const readJson = (path: string) => JSON.parse(readFileSync(join(root, path), "utf8")) as Record<string, any>

test("semantic contract registry declares every crossing with authority and lossiness", () => {
  const registry = readJson("contracts/vocabulary.json")
  expect(registry.layers).toEqual(expect.objectContaining({ public: expect.any(Array), runtime: expect.any(Array), kernel: expect.any(Array) }))
  for (const file of readdirSync(join(root, "contracts/crossings")).filter(name => name.endsWith(".json"))) {
    const contract = readJson(`contracts/crossings/${file}`)
    expect(contract.source).toEqual(expect.objectContaining({ layer: expect.any(String), authority: expect.any(String) }))
    expect(contract.target).toEqual(expect.objectContaining({ layer: expect.any(String), authority: expect.any(String) }))
    expect(contract.crossing).toEqual(expect.objectContaining({ verb: expect.any(String), function: expect.any(String) }))
    expect(contract.lossiness).toEqual(expect.any(String))
  }
})

test("forbidden crossing matrix rejects direct Public/Provider to Kernel paths", () => {
  const forbidden = readJson("contracts/forbidden-crossings.json")
  expect(forbidden["Public->Kernel"]).toBe("forbidden_direct_crossing")
  expect(forbidden["Provider->Kernel"]).toBe("forbidden_direct_crossing")
})
