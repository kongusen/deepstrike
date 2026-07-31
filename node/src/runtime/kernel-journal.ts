/**
 * `KernelJournal` — the durable transaction capability of the Canonical Kernel ABI (spec §9.1).
 *
 * This is the *authoritative* interface shape for all four SDKs (Node first, then Python/WASM/Rust).
 *
 * Three rules give this file its shape:
 *
 * 1. **Records are opaque.** core owns canonical serialization and hashing
 *    (`KernelRecord::record_bytes()` / `record_digest()` / `expected_head()`). The host stores the
 *    bytes verbatim and indexes them by the digest core handed it. A journal that re-serializes a
 *    record to recompute its hash would make "the host recomputed and disagreed" a reachable state;
 *    it is not one here.
 * 2. **CAS is a storage-layer primitive, not a read-compare-write sequence.** §9.1 requires a real
 *    atomic operation (file lock / `O_EXCL` chained naming / conditional database update).
 *    `InMemoryKernelJournal` is atomic only within one process and says so in its own type;
 *    `FileKernelJournal` is atomic across processes (see its class docs).
 * 3. **The journal's sequence space is `step_seq`** — the operation's record-chain position — and is
 *    completely independent of `SessionLog`'s business event `seq`. Pruning a journal prefix can
 *    never punch a hole in business event numbering (spec Task 8b, criterion 4).
 *
 * Failures are typed so a caller can tell "retry after rebuild" from "this storage is broken" from
 * "someone handed me a corrupt chain" — a durable-step wrapper must never publish effects on any of
 * them, and must never collapse them into one opaque `Error`.
 */

import { link, mkdir, open as openFile, readdir, readFile, rename, unlink } from "node:fs/promises"
import { randomUUID } from "node:crypto"
import { dirname, join } from "node:path"
import { KernelLogConflictError, KernelLogIntegrityError } from "./kernel-transaction-log.js"

export const MAX_CHAIN_POSITION = 1_000_000_000_000

/* ------------------------------------------------------------------ *
 * Errors
 * ------------------------------------------------------------------ */

/**
 * The CAS precondition did not hold: the journal head (or checkpoint pointer) moved.
 *
 * Retryable — the protocol response is `abort(token)` → re-read head → rebuild → replay the input
 * (spec §8.3, row "CAS conflict"). Extends the legacy `KernelLogConflictError` so existing callers
 * keep working while gaining the sharper type.
 */
export class JournalCasConflictError extends KernelLogConflictError {
  constructor(message: string) {
    super(message)
    this.name = "JournalCasConflictError"
  }
}

/**
 * The journal contents contradict themselves or the caller's claim: a broken digest chain, a
 * `step_seq` that does not follow its predecessor, a checkpoint whose `covered_head` does not match
 * the record at its `through_step_seq`. Never retryable — retrying replays the same contradiction.
 */
export class JournalIntegrityError extends KernelLogIntegrityError {
  constructor(message: string) {
    super(message)
    this.name = "JournalIntegrityError"
  }
}

/**
 * The storage layer failed (disk full, permission denied, hard links unsupported). Distinct from
 * both of the above: the journal state is *unknown*, not known-conflicting and not known-corrupt.
 */
export class JournalIoError extends Error {
  constructor(message: string, options?: { cause?: unknown }) {
    super(message, options)
    this.name = "JournalIoError"
  }
}

/* ------------------------------------------------------------------ *
 * Record shapes
 * ------------------------------------------------------------------ */

/** What a host hands the journal: core's opaque bytes plus the identity core assigned them. */
export interface JournalRecordInput {
  /** Chain position. Genesis (`ConfigureOperation`) is 0; every later record is `head + 1`. */
  step_seq: number
  /** core's `record_digest`. The journal indexes by it and never recomputes it. */
  record_digest: string
  /** core's `record_bytes`, stored verbatim. */
  record_bytes: Uint8Array
}

/** A stored record. `previous_record_digest` is the CAS precondition it was appended under. */
export interface JournalEntry extends JournalRecordInput {
  /** Absent on the genesis record only (spec §8.1: the first append's expected head is empty). */
  previous_record_digest?: string
}

/** The journal head: the last record of an operation's chain. */
export interface JournalHead {
  step_seq: number
  record_digest: string
}

export interface JournalAppendReceipt {
  step_seq: number
  record_digest: string
}

/* ------------------------------------------------------------------ *
 * Checkpoint shapes
 * ------------------------------------------------------------------ */

/** The host-persistable part of `kernel.checkpoint_candidate()` (spec §12.3). */
export interface CheckpointCandidate {
  /** Stable identity of this checkpoint; the CAS token a later install names as its predecessor. */
  checkpoint_id: string
  /** The chain position this checkpoint's logical state covers. */
  through_step_seq: number
  /** core's digest of the logical state. Opaque to the journal. */
  state_digest: string
  /** The serialized checkpoint blob, stored verbatim. */
  checkpoint_bytes: Uint8Array
}

export interface InstalledCheckpoint extends CheckpointCandidate {
  /** Monotonic install ordinal. `previous_checkpoint_id === undefined` installs ordinal 0. */
  ordinal: number
  /** The record digest at `through_step_seq` — verified at install, not required to still be head. */
  covered_head: string
  previous_checkpoint_id?: string
  /**
   * Whether `ackCheckpoint` has run. Prefix reclamation is gated on this: a checkpoint that is
   * installed but not acknowledged is recoverable-from but not prune-authorising (spec §12.3 rule 5/6).
   */
  acknowledged: boolean
}

export interface JournalPruneReceipt {
  /** Highest `step_seq` no longer retained. `-1` when nothing has ever been pruned. */
  pruned_through_step_seq: number
  pruned_count: number
}

/* ------------------------------------------------------------------ *
 * The capability
 * ------------------------------------------------------------------ */

/**
 * The durable transaction capability (spec §9.1). Deliberately *not* part of `SessionLog`: a custom
 * business-projection log must never be forced to masquerade as a transactional journal (§9.4).
 * One class may implement both — `InMemorySessionLog` does — but the interfaces stay separate.
 *
 * Guarantees an implementation owes (spec §9.1):
 * - strict ordering within an operation;
 * - a failed CAS never overwrites;
 * - record bytes preserved verbatim;
 * - checkpoint pointer advances monotonically;
 * - the checkpoint store verifies `covered_head` against `through_step_seq` but does **not** require
 *   it to still be the current transaction head (§22.14);
 * - checkpoint pointer and prefix pruning have an explicit acknowledgement boundary.
 */
export interface KernelJournal {
  /**
   * Atomically append `record` iff the operation's head is exactly `expectedHead`.
   *
   * @param expectedHead `undefined` starts the chain (genesis; `record.step_seq` must be 0).
   * @throws {JournalCasConflictError} head moved, or a genesis already exists.
   * @throws {JournalIntegrityError} `step_seq` does not follow the head.
   * @throws {JournalIoError} storage failure.
   */
  compareAndAppend(
    operationId: string,
    expectedHead: string | undefined,
    record: JournalRecordInput,
  ): Promise<JournalAppendReceipt>

  /** The current head, or `undefined` when the operation has no records and no pruned anchor. */
  head(operationId: string): Promise<JournalHead | undefined>

  /** Records with `step_seq >= fromStepSeq` (default: the whole retained chain), in chain order. */
  readFrom(operationId: string, fromStepSeq?: number): Promise<JournalEntry[]>

  /**
   * Records strictly after the record whose digest is `afterHead` — the digest-anchored cursor
   * §9.1 names `records_after(operation_id, checkpoint_head)`. `undefined` returns everything retained.
   *
   * @throws {JournalIntegrityError} `afterHead` names no retained record and no pruned anchor.
   */
  recordsAfter(operationId: string, afterHead?: string): Promise<JournalEntry[]>

  /**
   * Atomically install `checkpoint` iff the operation's checkpoint pointer is exactly
   * `previousCheckpointId`, and `coveredHead` is the record digest at `checkpoint.through_step_seq`.
   *
   * Per §22.14 this deliberately does **not** require `coveredHead` to still be the current
   * transaction head: transactions appended after the candidate was taken stay as tail.
   *
   * @param previousCheckpointId `undefined` installs the operation's first checkpoint.
   * @throws {JournalCasConflictError} the checkpoint pointer moved.
   * @throws {JournalIntegrityError} `coveredHead`/`through_step_seq` disagree with the chain, or
   *   `through_step_seq` would move the pointer backwards.
   */
  compareAndInstallCheckpoint(
    operationId: string,
    previousCheckpointId: string | undefined,
    coveredHead: string,
    checkpoint: CheckpointCandidate,
  ): Promise<InstalledCheckpoint>

  /** The highest-ordinal installed checkpoint, acknowledged or not. */
  latestCheckpoint(operationId: string): Promise<InstalledCheckpoint | undefined>

  /**
   * Record the durable acknowledgement that opens the prefix-reclamation boundary. Idempotent.
   *
   * @throws {JournalIntegrityError} no checkpoint with that id is installed.
   */
  ackCheckpoint(operationId: string, checkpointId: string): Promise<InstalledCheckpoint>

  /**
   * Reclaim the record prefix covered by the latest **acknowledged** checkpoint. A no-op while no
   * checkpoint is acknowledged. The pruned boundary is retained as an anchor so `head()` and the
   * next CAS still resolve on a fully-pruned chain.
   */
  pruneAckedPrefix(operationId: string): Promise<JournalPruneReceipt>

  /**
   * Durably stage one outbound WireEnvelope JSON for `operationId` until the matching record is
   * append-acked (or the attempt is abandoned). Overwrites any previous pending envelope.
   *
   * Required by Phase 6 host cutover (adjudication 5e.3): retries must replay byte-identical
   * envelopes — reminting `observed_at_ms` after a crash produces `DuplicateInputConflict`.
   */
  stageOutboundEnvelope(operationId: string, envelopeJson: string): Promise<void>

  /** Read the staged outbound envelope, if any. */
  readOutboundEnvelope(operationId: string): Promise<string | undefined>

  /** Clear the staged outbound envelope. Idempotent. */
  clearOutboundEnvelope(operationId: string): Promise<void>
}

/* ------------------------------------------------------------------ *
 * Shared validation
 * ------------------------------------------------------------------ */

function validateChainPosition(value: number, label: string): void {
  if (!Number.isSafeInteger(value) || value < 0 || value >= MAX_CHAIN_POSITION) {
    throw new JournalIntegrityError(
      `${label} must be a non-negative integer below ${MAX_CHAIN_POSITION}`,
    )
  }
}

function validateRecord(record: JournalRecordInput): void {
  validateChainPosition(record.step_seq, "journal record step_seq")
  if (!record.record_digest) throw new JournalIntegrityError("journal record requires a record_digest")
  if (!(record.record_bytes instanceof Uint8Array)) {
    throw new JournalIntegrityError("journal record requires opaque record_bytes")
  }
}

function validateCandidate(checkpoint: CheckpointCandidate): void {
  if (!checkpoint.checkpoint_id) throw new JournalIntegrityError("checkpoint requires a checkpoint_id")
  validateChainPosition(checkpoint.through_step_seq, "checkpoint through_step_seq")
  if (!checkpoint.state_digest) throw new JournalIntegrityError("checkpoint requires a state_digest")
  if (!(checkpoint.checkpoint_bytes instanceof Uint8Array)) {
    throw new JournalIntegrityError("checkpoint requires opaque checkpoint_bytes")
  }
}

/**
 * Check the CAS precondition against the observed head, and the chain position that follows from it.
 * Shared by both implementations so their conflict/integrity taxonomy cannot drift apart.
 */
function checkAppendPrecondition(
  head: JournalHead | undefined,
  expectedHead: string | undefined,
  record: JournalRecordInput,
): void {
  if (expectedHead === undefined) {
    if (head) {
      throw new JournalCasConflictError(
        "journal genesis append requires an empty chain, but the operation already has a head",
      )
    }
    if (record.step_seq !== 0) {
      throw new JournalIntegrityError("journal genesis record must have step_seq 0")
    }
    return
  }
  if (!head) {
    throw new JournalCasConflictError("journal compare-and-append expected a head, but the chain is empty")
  }
  if (head.record_digest !== expectedHead) {
    throw new JournalCasConflictError("journal head changed before compare-and-append")
  }
  if (record.step_seq !== head.step_seq + 1) {
    throw new JournalIntegrityError(
      `journal record step_seq ${record.step_seq} does not follow head step_seq ${head.step_seq}`,
    )
  }
}

/** Verify one contiguous run of entries links head-to-tail. */
function verifyChain(entries: readonly JournalEntry[]): void {
  for (let i = 1; i < entries.length; i++) {
    const previous = entries[i - 1]
    const entry = entries[i]
    if (entry.step_seq !== previous.step_seq + 1) {
      throw new JournalIntegrityError(
        `journal chain has a gap: step_seq ${entry.step_seq} follows ${previous.step_seq}`,
      )
    }
    if (entry.previous_record_digest !== previous.record_digest) {
      throw new JournalIntegrityError("journal chain digest linkage is not continuous")
    }
  }
}

/* ------------------------------------------------------------------ *
 * In-memory implementation
 * ------------------------------------------------------------------ */

interface PrunedAnchor {
  through_step_seq: number
  covered_head: string
}

interface OperationState {
  records: JournalEntry[]
  checkpoints: InstalledCheckpoint[]
  pruned?: PrunedAnchor
  /** Byte-identical WireEnvelope JSON awaiting append-ack (at most one per operation). */
  outboundEnvelope?: string
}

/**
 * **Single-process dev/test implementation.**
 *
 * CAS is genuinely atomic here — every check-then-mutate below runs to completion without an
 * intervening `await`, so no other task can interleave on a single-threaded runtime — but that
 * atomicity ends at the process boundary. Two processes sharing "the same" journal do not exist:
 * each has its own `Map`. Production hosts must supply a `KernelJournal` whose CAS is a real
 * storage-layer primitive (spec §9.1); `FileKernelJournal` is the reference for that.
 */
export class InMemoryKernelJournal implements KernelJournal {
  private readonly operations = new Map<string, OperationState>()

  private state(operationId: string): OperationState {
    let state = this.operations.get(operationId)
    if (!state) {
      state = { records: [], checkpoints: [] }
      this.operations.set(operationId, state)
    }
    return state
  }

  private headOf(state: OperationState): JournalHead | undefined {
    const last = state.records.at(-1)
    if (last) return { step_seq: last.step_seq, record_digest: last.record_digest }
    if (state.pruned) {
      return { step_seq: state.pruned.through_step_seq, record_digest: state.pruned.covered_head }
    }
    return undefined
  }

  async compareAndAppend(
    operationId: string,
    expectedHead: string | undefined,
    record: JournalRecordInput,
  ): Promise<JournalAppendReceipt> {
    validateRecord(record)
    // Atomic region: no `await` from here to the push.
    const state = this.state(operationId)
    checkAppendPrecondition(this.headOf(state), expectedHead, record)
    state.records.push({
      step_seq: record.step_seq,
      record_digest: record.record_digest,
      record_bytes: Uint8Array.from(record.record_bytes),
      ...(expectedHead === undefined ? {} : { previous_record_digest: expectedHead }),
    })
    return { step_seq: record.step_seq, record_digest: record.record_digest }
  }

  async head(operationId: string): Promise<JournalHead | undefined> {
    const state = this.operations.get(operationId)
    return state ? this.headOf(state) : undefined
  }

  async readFrom(operationId: string, fromStepSeq = 0): Promise<JournalEntry[]> {
    const records = this.operations.get(operationId)?.records ?? []
    // Retained records are dense and ordered, so the cursor is an index — not a scan. Callers hit
    // this once per durable step to fetch the head; a filter here would make a run quadratic.
    const base = records[0]?.step_seq ?? 0
    const entries = records.slice(Math.max(0, fromStepSeq - base))
    verifyChain(entries)
    return entries.map(entry => ({ ...entry, record_bytes: Uint8Array.from(entry.record_bytes) }))
  }

  async recordsAfter(operationId: string, afterHead?: string): Promise<JournalEntry[]> {
    if (afterHead === undefined) return this.readFrom(operationId, 0)
    const state = this.operations.get(operationId)
    const anchor = state?.records.find(entry => entry.record_digest === afterHead)
    if (anchor) return this.readFrom(operationId, anchor.step_seq + 1)
    if (state?.pruned && state.pruned.covered_head === afterHead) {
      return this.readFrom(operationId, state.pruned.through_step_seq + 1)
    }
    throw new JournalIntegrityError("journal cursor digest names no retained record")
  }

  async compareAndInstallCheckpoint(
    operationId: string,
    previousCheckpointId: string | undefined,
    coveredHead: string,
    checkpoint: CheckpointCandidate,
  ): Promise<InstalledCheckpoint> {
    validateCandidate(checkpoint)
    // Atomic region: no `await` from here to the push.
    const state = this.state(operationId)
    const latest = state.checkpoints.at(-1)
    checkCheckpointPrecondition(latest, previousCheckpointId, checkpoint)
    verifyCoveredHead(
      state.records.find(entry => entry.step_seq === checkpoint.through_step_seq),
      state.pruned,
      coveredHead,
      checkpoint.through_step_seq,
    )
    const installed: InstalledCheckpoint = {
      ...checkpoint,
      checkpoint_bytes: Uint8Array.from(checkpoint.checkpoint_bytes),
      ordinal: latest ? latest.ordinal + 1 : 0,
      covered_head: coveredHead,
      ...(previousCheckpointId === undefined ? {} : { previous_checkpoint_id: previousCheckpointId }),
      acknowledged: false,
    }
    state.checkpoints.push(installed)
    return { ...installed, checkpoint_bytes: Uint8Array.from(installed.checkpoint_bytes) }
  }

  async latestCheckpoint(operationId: string): Promise<InstalledCheckpoint | undefined> {
    const latest = this.operations.get(operationId)?.checkpoints.at(-1)
    return latest ? { ...latest, checkpoint_bytes: Uint8Array.from(latest.checkpoint_bytes) } : undefined
  }

  async ackCheckpoint(operationId: string, checkpointId: string): Promise<InstalledCheckpoint> {
    const installed = this.operations
      .get(operationId)
      ?.checkpoints.find(entry => entry.checkpoint_id === checkpointId)
    if (!installed) throw new JournalIntegrityError("cannot acknowledge an uninstalled checkpoint")
    installed.acknowledged = true
    return { ...installed, checkpoint_bytes: Uint8Array.from(installed.checkpoint_bytes) }
  }

  async pruneAckedPrefix(operationId: string): Promise<JournalPruneReceipt> {
    const state = this.operations.get(operationId)
    const acked = [...(state?.checkpoints ?? [])].reverse().find(entry => entry.acknowledged)
    if (!state || !acked) {
      return { pruned_through_step_seq: state?.pruned?.through_step_seq ?? -1, pruned_count: 0 }
    }
    const before = state.records.length
    state.records = state.records.filter(entry => entry.step_seq > acked.through_step_seq)
    if (!state.pruned || state.pruned.through_step_seq < acked.through_step_seq) {
      state.pruned = { through_step_seq: acked.through_step_seq, covered_head: acked.covered_head }
    }
    return {
      pruned_through_step_seq: state.pruned.through_step_seq,
      pruned_count: before - state.records.length,
    }
  }

  async stageOutboundEnvelope(operationId: string, envelopeJson: string): Promise<void> {
    if (!envelopeJson) throw new JournalIntegrityError("outbound envelope must not be empty")
    this.state(operationId).outboundEnvelope = envelopeJson
  }

  async readOutboundEnvelope(operationId: string): Promise<string | undefined> {
    return this.operations.get(operationId)?.outboundEnvelope
  }

  async clearOutboundEnvelope(operationId: string): Promise<void> {
    const state = this.operations.get(operationId)
    if (state) delete state.outboundEnvelope
  }
}

function checkCheckpointPrecondition(
  latest: InstalledCheckpoint | undefined,
  previousCheckpointId: string | undefined,
  checkpoint: CheckpointCandidate,
): void {
  if (previousCheckpointId === undefined) {
    if (latest) {
      throw new JournalCasConflictError(
        "checkpoint install without a predecessor requires an empty checkpoint pointer",
      )
    }
    return
  }
  if (!latest) {
    throw new JournalCasConflictError("checkpoint install named a predecessor, but none is installed")
  }
  if (latest.checkpoint_id !== previousCheckpointId) {
    throw new JournalCasConflictError("checkpoint pointer changed before compare-and-install")
  }
  if (checkpoint.through_step_seq < latest.through_step_seq) {
    throw new JournalIntegrityError("checkpoint pointer must advance monotonically")
  }
}

/**
 * §9.1: the store verifies that `covered_head` is the record digest at `through_step_seq`. It does
 * NOT check that this is still the current head — §22.14 rejects that, since it would serialise
 * checkpointing against ordinary transitions.
 */
function verifyCoveredHead(
  covered: JournalEntry | undefined,
  pruned: PrunedAnchor | undefined,
  coveredHead: string,
  throughStepSeq: number,
): void {
  if (covered) {
    if (covered.record_digest !== coveredHead) {
      throw new JournalIntegrityError(
        "checkpoint covered_head does not match the record at its through_step_seq",
      )
    }
    return
  }
  if (pruned && pruned.through_step_seq === throughStepSeq && pruned.covered_head === coveredHead) return
  throw new JournalIntegrityError("checkpoint through_step_seq names no retained record")
}

/* ------------------------------------------------------------------ *
 * File implementation
 * ------------------------------------------------------------------ */

const SEQ_DIGITS = 12
const RECORD_NAME = /^(\d{12})\.rec$/
const CHECKPOINT_NAME = /^(\d{12})\.ckpt$/
const ACK_NAME = /^(\d{12})\.ack$/

function pad(value: number): string {
  return String(value).padStart(SEQ_DIGITS, "0")
}

/** Filesystem-safe, injective encoding of an operation id. */
function safeSegment(value: string): string {
  return `op-${Buffer.from(value, "utf8").toString("base64url")}`
}

interface PersistedRecord {
  step_seq: number
  record_digest: string
  previous_record_digest?: string
  record_bytes: string
}

interface PersistedCheckpoint {
  ordinal: number
  checkpoint_id: string
  previous_checkpoint_id?: string
  covered_head: string
  through_step_seq: number
  state_digest: string
  checkpoint_bytes: string
}

/**
 * **Cross-process atomic reference implementation** of `KernelJournal` (spec Task 8b).
 *
 * The atomicity primitive is POSIX `link(2)`: content is written to a private temp file and fsynced,
 * then hard-linked into its final name. `link` fails with `EEXIST` if the name is taken, and it
 * publishes already-complete content — so a crash can never leave a half-written `.rec`, only an
 * orphan temp file that the naming rule ignores. The journal root must therefore live on a
 * filesystem that supports hard links; that requirement is the price of real CAS.
 *
 * **Why the record filename is `<step_seq>.rec` and contains no digest.** The filename *is* the
 * collision domain. Two writers racing on the same head both compute the same next `step_seq`, so
 * they contend for one name and exactly one wins. Folding a per-writer value (the new record's
 * digest) into the name would give the racers *different* names — both `link`s would succeed and the
 * chain would fork. Only the predecessor-determined part of the identity may appear in the name.
 *
 * The pre-`link` head check is not a TOCTOU hole: it can only *reject* an append that `link` would
 * have accepted (a stale `expectedHead` whose `step_seq` slot happens to be free), never accept one
 * `link` would have rejected. Every acceptance is still decided by the atomic `link`.
 *
 * Checkpoint installs use the same primitive on a separate ordinal space
 * (`<ordinal>.ckpt`), so two processes installing on the same predecessor also contend for one name.
 */
export class FileKernelJournal implements KernelJournal {
  constructor(private readonly root: string) {}

  private operationDir(operationId: string): string {
    return join(this.root, safeSegment(operationId))
  }

  private recordsDir(operationId: string): string {
    return join(this.operationDir(operationId), "records")
  }

  private checkpointsDir(operationId: string): string {
    return join(this.operationDir(operationId), "checkpoints")
  }

  private tmpDir(operationId: string): string {
    return join(this.operationDir(operationId), "tmp")
  }

  private prunedPath(operationId: string): string {
    return join(this.operationDir(operationId), "pruned.json")
  }

  /**
   * Write `payload` to a temp file, fsync it, then atomically claim `target` by hard link.
   *
   * @returns `false` when the name was already taken — i.e. a lost CAS race.
   */
  private async publish(operationId: string, target: string, payload: string): Promise<boolean> {
    const tmpDir = this.tmpDir(operationId)
    const tmpPath = join(tmpDir, `${randomUUID()}.tmp`)
    try {
      await mkdir(tmpDir, { recursive: true })
      const handle = await openFile(tmpPath, "wx")
      try {
        await handle.writeFile(payload, "utf8")
        await handle.sync()
      } finally {
        await handle.close()
      }
    } catch (err) {
      throw new JournalIoError("journal could not stage a durable record", { cause: err })
    }
    try {
      await link(tmpPath, target)
    } catch (err) {
      if ((err as { code?: string }).code === "EEXIST") {
        await unlink(tmpPath).catch(() => undefined)
        return false
      }
      await unlink(tmpPath).catch(() => undefined)
      throw new JournalIoError("journal could not publish a durable record", { cause: err })
    }
    await unlink(tmpPath).catch(() => undefined)
    await this.syncDir(dirname(target))
    return true
  }

  private async syncDir(dir: string): Promise<void> {
    try {
      const handle = await openFile(dir, "r")
      try {
        await handle.sync()
      } finally {
        await handle.close()
      }
    } catch {
      // Directory fsync is a durability nicety; some platforms refuse it. Never fail the append.
    }
  }

  /** Sorted `step_seq` values of the retained records. Anything not matching the rule is residue. */
  private async recordSeqs(operationId: string): Promise<number[]> {
    let names: string[]
    try {
      names = await readdir(this.recordsDir(operationId))
    } catch (err) {
      if ((err as { code?: string }).code === "ENOENT") return []
      throw new JournalIoError("journal could not list its records", { cause: err })
    }
    const seqs: number[] = []
    for (const name of names) {
      const match = RECORD_NAME.exec(name)
      if (match) seqs.push(Number(match[1]))
    }
    return seqs.sort((a, b) => a - b)
  }

  private async readRecord(operationId: string, stepSeq: number): Promise<JournalEntry | undefined> {
    let raw: string
    try {
      raw = await readFile(join(this.recordsDir(operationId), `${pad(stepSeq)}.rec`), "utf8")
    } catch (err) {
      if ((err as { code?: string }).code === "ENOENT") return undefined
      throw new JournalIoError("journal could not read a record", { cause: err })
    }
    let persisted: PersistedRecord
    try {
      persisted = JSON.parse(raw) as PersistedRecord
    } catch (err) {
      throw new JournalIntegrityError(`journal record ${pad(stepSeq)} is not readable: ${String(err)}`)
    }
    if (persisted.step_seq !== stepSeq) {
      throw new JournalIntegrityError(`journal record ${pad(stepSeq)} disagrees with its own step_seq`)
    }
    return {
      step_seq: persisted.step_seq,
      record_digest: persisted.record_digest,
      record_bytes: new Uint8Array(Buffer.from(persisted.record_bytes, "base64")),
      ...(persisted.previous_record_digest === undefined
        ? {}
        : { previous_record_digest: persisted.previous_record_digest }),
    }
  }

  private async prunedAnchor(operationId: string): Promise<PrunedAnchor | undefined> {
    try {
      return JSON.parse(await readFile(this.prunedPath(operationId), "utf8")) as PrunedAnchor
    } catch (err) {
      if ((err as { code?: string }).code === "ENOENT") return undefined
      throw new JournalIoError("journal could not read its pruned anchor", { cause: err })
    }
  }

  async head(operationId: string): Promise<JournalHead | undefined> {
    const seqs = await this.recordSeqs(operationId)
    const last = seqs.at(-1)
    if (last !== undefined) {
      const entry = await this.readRecord(operationId, last)
      if (entry) return { step_seq: entry.step_seq, record_digest: entry.record_digest }
    }
    const pruned = await this.prunedAnchor(operationId)
    return pruned ? { step_seq: pruned.through_step_seq, record_digest: pruned.covered_head } : undefined
  }

  async compareAndAppend(
    operationId: string,
    expectedHead: string | undefined,
    record: JournalRecordInput,
  ): Promise<JournalAppendReceipt> {
    validateRecord(record)
    checkAppendPrecondition(await this.head(operationId), expectedHead, record)

    const recordsDir = this.recordsDir(operationId)
    try {
      await mkdir(recordsDir, { recursive: true })
    } catch (err) {
      throw new JournalIoError("journal could not create its record directory", { cause: err })
    }
    const persisted: PersistedRecord = {
      step_seq: record.step_seq,
      record_digest: record.record_digest,
      ...(expectedHead === undefined ? {} : { previous_record_digest: expectedHead }),
      record_bytes: Buffer.from(record.record_bytes).toString("base64"),
    }
    const won = await this.publish(
      operationId,
      join(recordsDir, `${pad(record.step_seq)}.rec`),
      JSON.stringify(persisted),
    )
    if (!won) {
      throw new JournalCasConflictError(
        `journal step_seq ${record.step_seq} was claimed by a concurrent writer`,
      )
    }
    return { step_seq: record.step_seq, record_digest: record.record_digest }
  }

  async readFrom(operationId: string, fromStepSeq = 0): Promise<JournalEntry[]> {
    const entries: JournalEntry[] = []
    for (const seq of await this.recordSeqs(operationId)) {
      if (seq < fromStepSeq) continue
      const entry = await this.readRecord(operationId, seq)
      if (entry) entries.push(entry)
    }
    verifyChain(entries)
    return entries
  }

  async recordsAfter(operationId: string, afterHead?: string): Promise<JournalEntry[]> {
    if (afterHead === undefined) return this.readFrom(operationId, 0)
    for (const seq of await this.recordSeqs(operationId)) {
      const entry = await this.readRecord(operationId, seq)
      if (entry?.record_digest === afterHead) return this.readFrom(operationId, seq + 1)
    }
    const pruned = await this.prunedAnchor(operationId)
    if (pruned?.covered_head === afterHead) return this.readFrom(operationId, pruned.through_step_seq + 1)
    throw new JournalIntegrityError("journal cursor digest names no retained record")
  }

  /** Sorted ordinals of installed checkpoints. */
  private async checkpointOrdinals(operationId: string): Promise<number[]> {
    let names: string[]
    try {
      names = await readdir(this.checkpointsDir(operationId))
    } catch (err) {
      if ((err as { code?: string }).code === "ENOENT") return []
      throw new JournalIoError("journal could not list its checkpoints", { cause: err })
    }
    const ordinals: number[] = []
    for (const name of names) {
      const match = CHECKPOINT_NAME.exec(name)
      if (match) ordinals.push(Number(match[1]))
    }
    return ordinals.sort((a, b) => a - b)
  }

  private async ackedOrdinals(operationId: string): Promise<Set<number>> {
    let names: string[]
    try {
      names = await readdir(this.checkpointsDir(operationId))
    } catch (err) {
      if ((err as { code?: string }).code === "ENOENT") return new Set()
      throw new JournalIoError("journal could not list its checkpoints", { cause: err })
    }
    const acked = new Set<number>()
    for (const name of names) {
      const match = ACK_NAME.exec(name)
      if (match) acked.add(Number(match[1]))
    }
    return acked
  }

  private async readCheckpoint(
    operationId: string,
    ordinal: number,
    acked?: Set<number>,
  ): Promise<InstalledCheckpoint | undefined> {
    let raw: string
    try {
      raw = await readFile(join(this.checkpointsDir(operationId), `${pad(ordinal)}.ckpt`), "utf8")
    } catch (err) {
      if ((err as { code?: string }).code === "ENOENT") return undefined
      throw new JournalIoError("journal could not read a checkpoint", { cause: err })
    }
    let persisted: PersistedCheckpoint
    try {
      persisted = JSON.parse(raw) as PersistedCheckpoint
    } catch (err) {
      throw new JournalIntegrityError(`checkpoint ${pad(ordinal)} is not readable: ${String(err)}`)
    }
    const acknowledged = (acked ?? (await this.ackedOrdinals(operationId))).has(ordinal)
    return {
      ordinal: persisted.ordinal,
      checkpoint_id: persisted.checkpoint_id,
      ...(persisted.previous_checkpoint_id === undefined
        ? {}
        : { previous_checkpoint_id: persisted.previous_checkpoint_id }),
      covered_head: persisted.covered_head,
      through_step_seq: persisted.through_step_seq,
      state_digest: persisted.state_digest,
      checkpoint_bytes: new Uint8Array(Buffer.from(persisted.checkpoint_bytes, "base64")),
      acknowledged,
    }
  }

  async latestCheckpoint(operationId: string): Promise<InstalledCheckpoint | undefined> {
    const ordinals = await this.checkpointOrdinals(operationId)
    const last = ordinals.at(-1)
    return last === undefined ? undefined : this.readCheckpoint(operationId, last)
  }

  async compareAndInstallCheckpoint(
    operationId: string,
    previousCheckpointId: string | undefined,
    coveredHead: string,
    checkpoint: CheckpointCandidate,
  ): Promise<InstalledCheckpoint> {
    validateCandidate(checkpoint)
    const latest = await this.latestCheckpoint(operationId)
    checkCheckpointPrecondition(latest, previousCheckpointId, checkpoint)
    verifyCoveredHead(
      await this.readRecord(operationId, checkpoint.through_step_seq),
      await this.prunedAnchor(operationId),
      coveredHead,
      checkpoint.through_step_seq,
    )

    const checkpointsDir = this.checkpointsDir(operationId)
    try {
      await mkdir(checkpointsDir, { recursive: true })
    } catch (err) {
      throw new JournalIoError("journal could not create its checkpoint directory", { cause: err })
    }
    const ordinal = latest ? latest.ordinal + 1 : 0
    const persisted: PersistedCheckpoint = {
      ordinal,
      checkpoint_id: checkpoint.checkpoint_id,
      ...(previousCheckpointId === undefined ? {} : { previous_checkpoint_id: previousCheckpointId }),
      covered_head: coveredHead,
      through_step_seq: checkpoint.through_step_seq,
      state_digest: checkpoint.state_digest,
      checkpoint_bytes: Buffer.from(checkpoint.checkpoint_bytes).toString("base64"),
    }
    const won = await this.publish(
      operationId,
      join(checkpointsDir, `${pad(ordinal)}.ckpt`),
      JSON.stringify(persisted),
    )
    if (!won) {
      throw new JournalCasConflictError(
        `checkpoint ordinal ${ordinal} was claimed by a concurrent installer`,
      )
    }
    return {
      ...checkpoint,
      checkpoint_bytes: Uint8Array.from(checkpoint.checkpoint_bytes),
      ordinal,
      covered_head: coveredHead,
      ...(previousCheckpointId === undefined ? {} : { previous_checkpoint_id: previousCheckpointId }),
      acknowledged: false,
    }
  }

  async ackCheckpoint(operationId: string, checkpointId: string): Promise<InstalledCheckpoint> {
    for (const ordinal of (await this.checkpointOrdinals(operationId)).reverse()) {
      const installed = await this.readCheckpoint(operationId, ordinal)
      if (!installed || installed.checkpoint_id !== checkpointId) continue
      if (!installed.acknowledged) {
        // `publish` returning false means another process already acknowledged it — idempotent.
        await this.publish(
          operationId,
          join(this.checkpointsDir(operationId), `${pad(ordinal)}.ack`),
          JSON.stringify({ ordinal, checkpoint_id: checkpointId }),
        )
      }
      return { ...installed, acknowledged: true }
    }
    throw new JournalIntegrityError("cannot acknowledge an uninstalled checkpoint")
  }

  async pruneAckedPrefix(operationId: string): Promise<JournalPruneReceipt> {
    const acked = await this.ackedOrdinals(operationId)
    const existing = await this.prunedAnchor(operationId)
    let boundary: InstalledCheckpoint | undefined
    for (const ordinal of (await this.checkpointOrdinals(operationId)).reverse()) {
      if (!acked.has(ordinal)) continue
      boundary = await this.readCheckpoint(operationId, ordinal, acked)
      break
    }
    if (!boundary) {
      return { pruned_through_step_seq: existing?.through_step_seq ?? -1, pruned_count: 0 }
    }
    // Anchor first, delete second: a crash mid-prune leaves a resolvable head either way.
    if (!existing || existing.through_step_seq < boundary.through_step_seq) {
      await this.writeAnchor(operationId, {
        through_step_seq: boundary.through_step_seq,
        covered_head: boundary.covered_head,
      })
    }
    let pruned = 0
    for (const seq of await this.recordSeqs(operationId)) {
      if (seq > boundary.through_step_seq) break
      await unlink(join(this.recordsDir(operationId), `${pad(seq)}.rec`)).catch(() => undefined)
      pruned++
    }
    return {
      pruned_through_step_seq: Math.max(boundary.through_step_seq, existing?.through_step_seq ?? -1),
      pruned_count: pruned,
    }
  }

  /** The anchor only ever moves forward, so an atomic overwriting `rename` is the right primitive. */
  private async writeAnchor(operationId: string, anchor: PrunedAnchor): Promise<void> {
    const tmpDir = this.tmpDir(operationId)
    const tmpPath = join(tmpDir, `${randomUUID()}.tmp`)
    try {
      await mkdir(tmpDir, { recursive: true })
      const handle = await openFile(tmpPath, "wx")
      try {
        await handle.writeFile(JSON.stringify(anchor), "utf8")
        await handle.sync()
      } finally {
        await handle.close()
      }
      await rename(tmpPath, this.prunedPath(operationId))
    } catch (err) {
      await unlink(tmpPath).catch(() => undefined)
      throw new JournalIoError("journal could not record its pruned anchor", { cause: err })
    }
  }

  private outboundPath(operationId: string): string {
    return join(this.operationDir(operationId), "outbound.json")
  }

  /** Overwriteable durable blob — same write-tmp→fsync→rename pattern as the pruned anchor. */
  private async writeReplaceable(operationId: string, target: string, payload: string): Promise<void> {
    const tmpDir = this.tmpDir(operationId)
    const tmpPath = join(tmpDir, `${randomUUID()}.tmp`)
    try {
      await mkdir(tmpDir, { recursive: true })
      const handle = await openFile(tmpPath, "wx")
      try {
        await handle.writeFile(payload, "utf8")
        await handle.sync()
      } finally {
        await handle.close()
      }
      await rename(tmpPath, target)
      await this.syncDir(dirname(target))
    } catch (err) {
      await unlink(tmpPath).catch(() => undefined)
      throw new JournalIoError("journal could not stage a durable outbound envelope", { cause: err })
    }
  }

  async stageOutboundEnvelope(operationId: string, envelopeJson: string): Promise<void> {
    if (!envelopeJson) throw new JournalIntegrityError("outbound envelope must not be empty")
    try {
      await mkdir(this.operationDir(operationId), { recursive: true })
    } catch (err) {
      throw new JournalIoError("journal could not create its operation directory", { cause: err })
    }
    await this.writeReplaceable(operationId, this.outboundPath(operationId), envelopeJson)
  }

  async readOutboundEnvelope(operationId: string): Promise<string | undefined> {
    try {
      return await readFile(this.outboundPath(operationId), "utf8")
    } catch (err) {
      if ((err as { code?: string }).code === "ENOENT") return undefined
      throw new JournalIoError("journal could not read its outbound envelope", { cause: err })
    }
  }

  async clearOutboundEnvelope(operationId: string): Promise<void> {
    try {
      await unlink(this.outboundPath(operationId))
    } catch (err) {
      if ((err as { code?: string }).code === "ENOENT") return
      throw new JournalIoError("journal could not clear its outbound envelope", { cause: err })
    }
  }
}
