import * as root from "../src/index.js"
import * as advanced from "../src/advanced.js"

test("WASM root exposes semantic Agent surface only", () => {
  expect(root).toHaveProperty("Agent")
  expect(root).toHaveProperty("createAgent")
  expect(root).not.toHaveProperty("RuntimeRunner")
  expect(root).not.toHaveProperty("InMemorySessionLog")
  expect(root).not.toHaveProperty("KernelJournal")
  expect(root).not.toHaveProperty("EvolutionRuntime")
})

test("WASM advanced surface owns runtime machinery", () => {
  expect(advanced).toHaveProperty("RuntimeRunner")
  expect(advanced).toHaveProperty("InMemorySessionLog")
  expect(advanced).toHaveProperty("EvolutionRuntime")
})
