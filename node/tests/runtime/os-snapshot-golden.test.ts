import { readFileSync } from "node:fs"
import { dirname, join } from "node:path"
import { fileURLToPath } from "node:url"
import {
  rebuildOsSnapshotFromSessionEvents,
  sessionLogHasRequiredCategories,
} from "../../src/runtime/os-snapshot.js"
import type { SessionEvent } from "../../src/runtime/session-log.js"

const FIXTURES = join(fileURLToPath(new URL("../../../tests/fixtures/session", import.meta.url)))

function loadJson<T>(name: string): T {
  return JSON.parse(readFileSync(join(FIXTURES, name), "utf8")) as T
}

type SnapFixture = {
  last_suspend?: { turn: number; reason: string; pending_calls: string[] }
  last_resumed_turn?: number
  process_by_agent: Array<{ turn: number; agent_id: string; parent_session_id: string; state: string }>
  budget_exceeded: Array<{ turn: number; budget: string }>
  signals: Array<{ turn: number; signal_id: string; disposition: string; queue_depth: number }>
  page_out_count: number
  page_in_count: number
  tool_gated_count: number
}

function expectSnapMatchesFixture(
  snap: ReturnType<typeof rebuildOsSnapshotFromSessionEvents>,
  raw: SnapFixture,
) {
  expect(snap.lastSuspend).toEqual(raw.last_suspend)
  expect(snap.lastResumedTurn).toBe(raw.last_resumed_turn)
  expect(snap.processByAgent).toEqual(raw.process_by_agent.map(p => ({
    turn: p.turn,
    agent_id: p.agent_id,
    parent_session_id: p.parent_session_id,
    state: p.state,
  })))
  expect(snap.budgetExceeded).toEqual(raw.budget_exceeded)
  expect(snap.signals).toEqual(raw.signals)
  expect(snap.pageOutCount).toBe(raw.page_out_count)
  expect(snap.pageInCount).toBe(raw.page_in_count)
  expect(snap.toolGatedCount).toBe(raw.tool_gated_count)
}

describe("OS snapshot golden fixtures (Phase 6)", () => {
  it("spawn lifecycle session events → OsSnapshot", () => {
    const events = loadJson<SessionEvent[]>("events_spawn_lifecycle.json")
    expect(sessionLogHasRequiredCategories(events)).toBe(true)
    const snap = rebuildOsSnapshotFromSessionEvents(events)
    expectSnapMatchesFixture(snap, loadJson<SnapFixture>("os_snapshot_spawn_lifecycle.json"))
  })

  it("ask_user governance session events → OsSnapshot", () => {
    const events = loadJson<SessionEvent[]>("events_ask_user.json")
    expect(sessionLogHasRequiredCategories(events)).toBe(true)
    const snap = rebuildOsSnapshotFromSessionEvents(events)
    expectSnapMatchesFixture(snap, loadJson<SnapFixture>("os_snapshot_ask_user.json"))
  })
})
