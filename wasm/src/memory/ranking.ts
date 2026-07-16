export interface RankableMemory<T> {
  value: T
  searchableText: string
  updatedAt: number
  /** Times this record has been recalled — a proven-useful record ranks slightly higher. */
  recallCount: number
  /** Optional day-based TTL. Records past it are discounted, not dropped (host owns hard deletion). */
  ttlDays?: number
  insertionIndex: number
}

/** One ranked hit with a genuine relevance score in [0,1] and a human-readable rationale. */
export interface RankedMemory<T> {
  value: T
  score: number
  why: string
}

export interface RankOptions {
  /** Wall-clock reference for the staleness discount. Omit to disable TTL/staleness scoring. */
  nowMs?: number
  /** Age (days) past which a record starts losing relevance. */
  staleWarningDays?: number
}

function terms(text: string): Set<string> {
  const result = new Set<string>()
  for (const segment of text.toLocaleLowerCase().match(/[\p{L}\p{N}]+/gu) ?? []) {
    result.add(segment)
    if (/\p{Script=Han}/u.test(segment)) {
      const characters = [...segment]
      for (let index = 0; index + 1 < characters.length; index++) {
        result.add(characters[index] + characters[index + 1])
      }
    }
  }
  return result
}

const DAY_MS = 86_400_000

/** Staleness discount in [0,1): grows once a record is older than the warning window, and jumps
 *  past its TTL. Clock-based, so it lives host-side (the kernel owns no wall clock). */
function stalenessPenalty(updatedAt: number, ttlDays: number | undefined, opts: RankOptions): number {
  if (opts.nowMs === undefined) return 0
  const ageDays = Math.max(0, (opts.nowMs - updatedAt) / DAY_MS)
  let penalty = 0
  if (opts.staleWarningDays !== undefined && ageDays > opts.staleWarningDays) {
    penalty += Math.min(0.3, 0.05 * (ageDays - opts.staleWarningDays))
  }
  if (ttlDays !== undefined && ageDays > ttlDays) {
    penalty += 0.4
  }
  return Math.min(0.9, penalty)
}

/** Small proven-usefulness boost from recall history: log-shaped, capped so it never overturns a
 *  clearly more lexically relevant record. */
function recallBoost(recallCount: number): number {
  if (recallCount <= 0) return 0
  return Math.min(0.15, 0.05 * Math.log2(1 + recallCount))
}

/**
 * Rank memories without embeddings or provider calls, returning a genuine relevance score in [0,1].
 *
 * The score is lexical overlap (distinct query terms present, as a fraction) as the dominant term,
 * lifted slightly by recall history and lowered by TTL/staleness. Recency and insertion order break
 * ties. A non-empty query never returns unrelated entries (lexical overlap zero ⇒ filtered out).
 *
 * The score is relevance, deliberately distinct from the record's stored confidence.
 */
export function rankMemories<T>(
  query: string,
  candidates: Array<RankableMemory<T>>,
  topK: number,
  opts: RankOptions = {},
): Array<RankedMemory<T>> {
  const queryTerms = terms(query)
  const limit = Math.max(0, Math.floor(topK))
  if (limit === 0) return []

  return candidates
    .map(candidate => {
      const candidateTerms = terms(candidate.searchableText)
      let lexicalMatches = 0
      for (const term of queryTerms) {
        if (candidateTerms.has(term)) lexicalMatches++
      }
      const lexicalFraction = queryTerms.size === 0 ? 0 : lexicalMatches / queryTerms.size
      const penalty = stalenessPenalty(candidate.updatedAt, candidate.ttlDays, opts)
      const boost = recallBoost(candidate.recallCount)
      const score = Math.max(0, Math.min(1, lexicalFraction * 0.85 + boost - penalty))
      const why = queryTerms.size === 0
        ? "no query terms; insertion order"
        : `lexical ${lexicalMatches}/${queryTerms.size}`
          + (boost > 0 ? `, recall×${candidate.recallCount}` : "")
          + (penalty > 0 ? `, stale −${penalty.toFixed(2)}` : "")
      return { ...candidate, lexicalMatches, score, why }
    })
    .filter(candidate => queryTerms.size === 0 || candidate.lexicalMatches > 0)
    .sort((a, b) =>
      b.score - a.score
      || b.lexicalMatches - a.lexicalMatches
      || b.updatedAt - a.updatedAt
      || a.insertionIndex - b.insertionIndex,
    )
    .slice(0, limit)
    .map(candidate => ({ value: candidate.value, score: candidate.score, why: candidate.why }))
}
