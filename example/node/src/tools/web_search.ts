import { tool } from "@deepstrike/sdk"

interface SearchResult {
  title: string
  url: string
  snippet: string
}

async function searchTavily(query: string, topK: number): Promise<SearchResult[]> {
  const res = await fetch("https://api.tavily.com/search", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ api_key: process.env.TAVILY_API_KEY, query, max_results: topK }),
    signal: AbortSignal.timeout(10_000),
  })
  const data = await res.json() as { results?: { title: string; url: string; content: string }[] }
  return (data.results ?? []).map(r => ({ title: r.title, url: r.url, snippet: r.content?.slice(0, 300) ?? "" }))
}

async function searchSerpApi(query: string, topK: number): Promise<SearchResult[]> {
  const url = `https://serpapi.com/search.json?q=${encodeURIComponent(query)}&num=${topK}&api_key=${process.env.SERPAPI_API_KEY}`
  const res = await fetch(url, { signal: AbortSignal.timeout(10_000) })
  const data = await res.json() as { organic_results?: { title: string; link: string; snippet: string }[] }
  return (data.organic_results ?? []).map(r => ({ title: r.title, url: r.link, snippet: r.snippet ?? "" }))
}

async function searchDDG(query: string, topK: number): Promise<SearchResult[]> {
  const url = `https://api.duckduckgo.com/?q=${encodeURIComponent(query)}&format=json&no_redirect=1&no_html=1`
  const res = await fetch(url, { signal: AbortSignal.timeout(10_000) })
  const data = await res.json() as {
    AbstractURL?: string; Heading?: string; Abstract?: string
    RelatedTopics?: { FirstURL?: string; Text?: string }[]
  }
  const results: SearchResult[] = []
  if (data.AbstractURL) {
    results.push({ title: data.Heading ?? "", url: data.AbstractURL, snippet: data.Abstract ?? "" })
  }
  for (const r of (data.RelatedTopics ?? []).slice(0, topK - 1)) {
    if (r.FirstURL) results.push({ title: "", url: r.FirstURL, snippet: r.Text ?? "" })
  }
  return results.slice(0, topK)
}

export const webSearch = tool(
  "web_search",
  "Search the web for a query and return top results with URLs",
  {
    type: "object",
    properties: {
      query: { type: "string", description: "Search query" },
      topK: { type: "number", description: "Max results (default 5)" },
    },
    required: ["query"],
  },
  async ({ query, topK = 5 }) => {
    const q = String(query)
    const k = Number(topK)
    try {
      let results: SearchResult[]
      if (process.env.TAVILY_API_KEY) results = await searchTavily(q, k)
      else if (process.env.SERPAPI_API_KEY) results = await searchSerpApi(q, k)
      else results = await searchDDG(q, k)

      if (!results.length) return "No results found."
      return results.map((r, i) => `${i + 1}. ${r.title}\n   ${r.url}\n   ${r.snippet}`).join("\n\n")
    } catch (err) {
      return `Search failed: ${String(err)}`
    }
  },
)
