export type NoteType = "idea" | "article" | "task" | "reference" | "insight" | "research"
export type NoteSource = "personal" | "community"

export interface Note {
  id: string
  type: NoteType
  tags: string[]
  summary: string
  connections: string[]
  source: NoteSource
  contributor?: string
  raw: string
  url?: string
  qualityScore: number
  createdAt: number
}
