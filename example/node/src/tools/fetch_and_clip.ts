import { tool } from "@deepstrike/sdk"

const BLOCK_DOMAINS = ["twitter.com", "x.com", "facebook.com", "tiktok.com", "instagram.com", "weibo.com"]
const CLIP_BYTES = 6 * 1024

export const fetchAndClip = tool(
  "fetch_and_clip",
  "Fetch a URL and return its main text content, clipped to 6 KB",
  {
    type: "object",
    properties: {
      url: { type: "string", description: "URL to fetch" },
    },
    required: ["url"],
  },
  async ({ url }) => {
    const urlStr = String(url)
    try {
      const { hostname } = new URL(urlStr)
      if (BLOCK_DOMAINS.some(d => hostname.includes(d))) {
        return JSON.stringify({ error: `Domain blocked by governance policy: ${hostname}` })
      }
    } catch {
      return JSON.stringify({ error: "Invalid URL" })
    }

    const jinaKey = process.env.JINA_API_KEY
    const fetchUrl = jinaKey ? `https://r.jina.ai/${urlStr}` : urlStr
    const headers: Record<string, string> = jinaKey
      ? { Authorization: `Bearer ${jinaKey}`, Accept: "text/plain" }
      : { "User-Agent": "Mozilla/5.0 (compatible; FlashNote/0.1)" }

    try {
      const res = await fetch(fetchUrl, { headers, signal: AbortSignal.timeout(15_000) })
      if (!res.ok) return JSON.stringify({ error: `HTTP ${res.status}` })
      const text = await res.text()
      const clipped = Buffer.from(text).slice(0, CLIP_BYTES).toString()
      return clipped.length < text.length ? `${clipped}\n\n[clipped at 6 KB]` : clipped
    } catch (err) {
      return JSON.stringify({ error: String(err) })
    }
  },
)
