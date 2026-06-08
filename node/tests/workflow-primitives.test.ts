import { createTournament, createLoopUntilDone } from "../src/kernel.js"

describe("Tournament (single-elimination, pairwise judging)", () => {
  it("emits round-batched match-ups and resolves a winner", () => {
    const t = createTournament(["a", "b", "c", "d"])

    const r1 = t.start()
    expect(r1.kind).toBe("judgeRound")
    expect(r1.round).toBe(1)
    expect(r1.matches).toEqual([
      { id: 0, left: "a", right: "b" },
      { id: 1, left: "c", right: "d" },
    ])

    const r2 = t.feedRound(["a", "d"])
    expect(r2.kind).toBe("judgeRound")
    expect(r2.round).toBe(2)
    expect(r2.matches).toEqual([{ id: 0, left: "a", right: "d" }])

    const done = t.feedRound(["d"])
    expect(done.kind).toBe("done")
    expect(done.winner).toBe("d")
    expect(done.roundsUsed).toBe(2)
    expect(t.isDone()).toBe(true)
  })

  it("advances an odd entrant via a bye", () => {
    const t = createTournament(["a", "b", "c"])
    const r1 = t.start()
    expect(r1.matches).toEqual([{ id: 0, left: "a", right: "b" }])
    const r2 = t.feedRound(["a"]) // c got a bye
    expect(r2.matches).toEqual([{ id: 0, left: "a", right: "c" }])
    expect(t.feedRound(["c"]).winner).toBe("c")
  })

  it("a single entrant wins immediately", () => {
    const t = createTournament(["solo"])
    const done = t.start()
    expect(done.kind).toBe("done")
    expect(done.winner).toBe("solo")
    expect(done.roundsUsed).toBe(0)
  })

  it("throws on empty entrants", () => {
    expect(() => createTournament([])).toThrow()
  })

  it("throws on a wrong winner count", () => {
    const t = createTournament(["a", "b", "c", "d"])
    t.start()
    expect(() => t.feedRound(["a"])).toThrow()
  })
})

describe("LoopUntilDone (stop predicates + backstop)", () => {
  it("stops on no new findings", () => {
    const l = createLoopUntilDone([{ kind: "noNewFindings" }])
    expect(l.start()).toEqual({ kind: "spawn", round: 1 })
    expect(l.feed({ newFindings: 3, errors: 0 })).toEqual({ kind: "spawn", round: 2 })
    expect(l.feed({ newFindings: 0, errors: 9 })).toEqual({
      kind: "done",
      roundsUsed: 2,
      reason: "noNewFindings",
    })
    expect(l.isDone()).toBe(true)
  })

  it("caps via an explicit maxRounds", () => {
    const l = createLoopUntilDone([{ kind: "maxRounds", maxRounds: 2 }])
    l.start()
    expect(l.feed({ newFindings: 1, errors: 1 })).toEqual({ kind: "spawn", round: 2 })
    expect(l.feed({ newFindings: 1, errors: 1 })).toEqual({
      kind: "done",
      roundsUsed: 2,
      reason: "maxRounds",
    })
  })

  it("the first configured condition wins", () => {
    const l = createLoopUntilDone([{ kind: "noNewFindings" }, { kind: "noErrors" }])
    l.start()
    expect(l.feed({ newFindings: 0, errors: 0 }).reason).toBe("noNewFindings")
  })

  it("throws when maxRounds lacks a value", () => {
    expect(() => createLoopUntilDone([{ kind: "maxRounds" }])).toThrow()
  })
})
