interface RankableMemory<T> {
  value: T
  searchableText: string
  updatedAt: number
  insertionIndex: number
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

/**
 * Rank memories without clocks, embeddings, or provider calls.
 *
 * Relevance is the number of distinct query terms present in the candidate. Recency only breaks
 * relevance ties; insertion order is the final stable tie-breaker. A non-empty query never falls
 * back to unrelated entries.
 */
export function rankMemories<T>(
  query: string,
  candidates: Array<RankableMemory<T>>,
  topK: number,
): T[] {
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
      return { ...candidate, lexicalMatches }
    })
    .filter(candidate => queryTerms.size === 0 || candidate.lexicalMatches > 0)
    .sort((a, b) =>
      b.lexicalMatches - a.lexicalMatches
      || b.updatedAt - a.updatedAt
      || a.insertionIndex - b.insertionIndex,
    )
    .slice(0, limit)
    .map(candidate => candidate.value)
}
