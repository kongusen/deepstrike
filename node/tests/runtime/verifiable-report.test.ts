import {
  VERIFIABLE_FORK_SCHEMA,
  VERIFIABLE_REPORT_SCHEMA,
  assertVerifiableReportSchema,
  createVerifiableRuntimeAdapter,
  VerifiableOperation,
} from "../../src/runtime/verifiable-report.js"

describe("0.2.69 verifiable report mirror", () => {
  test("uses the Rust report and fork schema names", () => {
    expect(VERIFIABLE_REPORT_SCHEMA).toBe("verifiable-report/v2")
    expect(VERIFIABLE_FORK_SCHEMA).toBe("verifiable-fork/v2")
    expect(() => assertVerifiableReportSchema({ schema: VERIFIABLE_REPORT_SCHEMA })).not.toThrow()
    expect(() => assertVerifiableReportSchema({ schema: "other/v1" })).toThrow()
  })

  test("delegates framework operations through one adapter", () => {
    const calls: string[] = []
    const adapter = {
      inspect: () => { calls.push("inspect"); return { schema: VERIFIABLE_REPORT_SCHEMA, command: "inspect", operation_id: "op" } },
      verify: () => { calls.push("verify"); return { schema: VERIFIABLE_REPORT_SCHEMA, command: "verify", operation_id: "op" } },
      replay: () => { calls.push("replay"); return { schema: VERIFIABLE_REPORT_SCHEMA, command: "replay", operation_id: "op" } },
      prepareFork: () => { calls.push("fork"); return { schema: VERIFIABLE_FORK_SCHEMA, operation_id: "op", at_step: "1", parent_record_digest: "sha256:p", parent_input_id: "in", source_records: 1 } },
    }
    const operation = new VerifiableOperation("op", { journal: [] }, adapter)
    expect(operation.inspect()).toMatchObject({ command: "inspect" })
    expect(operation.verify()).toMatchObject({ command: "verify" })
    expect(operation.replay()).toMatchObject({ command: "replay" })
    expect(operation.prepareFork(1).operation_id).toBe("op")
    expect(calls).toEqual(["inspect", "verify", "replay", "fork"])
  })

  test("encodes evidence for the Rust JSON bridge", () => {
    const requests: string[] = []
    const adapter = createVerifiableRuntimeAdapter((request) => {
      requests.push(request)
      return JSON.stringify({ schema: VERIFIABLE_REPORT_SCHEMA, command: "inspect", operation_id: "op" })
    })
    const report = new VerifiableOperation("op", { journal: [new Uint8Array([1, 2])] }, adapter).inspect()
    expect(report.operation_id).toBe("op")
    expect(JSON.parse(requests[0]).evidence.journal).toEqual([[1, 2]])
  })
})
