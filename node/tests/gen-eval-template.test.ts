/**
 * #6 (0.5.0 fold): the `genEval` workflow template — a worker + a bias-resistant Verify eval node
 * carrying the kernel's verdict output_schema (the EvalPipeline successor). Mirrors the kernel
 * `gen_eval` shape test + guards that the SDK and kernel verdict schemas stay single-sourced.
 */
import { genEval } from "../src/types/agent.js"
import { getKernel } from "../src/kernel.js"

describe("genEval template", () => {
  it("builds a worker + a bias-resistant verify eval node with the verdict schema", () => {
    const spec = genEval("implement feature", "score against criteria", true)
    expect(spec.nodes).toHaveLength(2)

    const worker = spec.nodes[0]
    expect(worker.role).toBe("implement")
    expect(worker.dependsOn ?? []).toEqual([])

    const evalNode = spec.nodes[1]
    expect(evalNode.role).toBe("verify")
    expect(evalNode.isolation).toBe("read_only")
    expect(evalNode.contextInheritance).toBe("none") // bias-resistant
    expect(evalNode.dependsOn).toEqual([0])
    expect(evalNode.outputSchema).toBeDefined()
    const schema = evalNode.outputSchema as Record<string, any>
    expect(schema.properties.passed).toBeDefined()
    expect(schema.properties.skill).toBeDefined() // extractSkillOnPass=true
  })

  it("drops the skill property when extraction is off", () => {
    const spec = genEval("w", "e", false)
    const schema = spec.nodes[1].outputSchema as Record<string, any>
    expect(schema.properties.skill).toBeUndefined()
  })

  it("uses the kernel's verdict schema verbatim (single source, no drift)", () => {
    const spec = genEval("w", "e", true)
    expect(spec.nodes[1].outputSchema).toEqual(JSON.parse(getKernel().verdictOutputSchema(true)))
  })
})
