import { readSkillFile, scanSkillDir } from "./skills/loader.js"

export interface SkillResource { name: string; uri?: string; content?: string; metadata?: Record<string, unknown> }
export interface SkillScript { name: string; command: string; description?: string; metadata?: Record<string, unknown> }
export type SkillTool = string | { name: string; description?: string; inputSchema?: Record<string, unknown> }
export type SkillMCPServer = string | { name: string; transport: Record<string, unknown> }
export type SkillKnowledge = string | { name: string; content?: string; source?: unknown }

/** spc_001 §2.4: public Skill contract, built directly on `SKILL.md`-style frontmatter files. */
export interface Skill {
  name: string
  description?: string
  instructions?: string
  resources?: SkillResource[]
  scripts?: SkillScript[]
  tools?: SkillTool[]
  mcpServers?: SkillMCPServer[]
  knowledge?: SkillKnowledge[]
  metadata?: Record<string, unknown>
  providerOptions?: Record<string, unknown>
}

/** Loads one skill by name from a skill directory, reusing the existing frontmatter scanner and
 *  body reader — does not reimplement directory scanning. Returns `null` if the skill file is
 *  absent (mirrors `readSkillFile`'s own not-found signal). */
export async function loadSkill(skillDir: string, name: string): Promise<Skill | null> {
  const body = await readSkillFile(skillDir, name)
  if (body === null) return null
  const metas = await scanSkillDir(skillDir)
  const meta = metas.find(m => m.name === name)
  return {
    name: meta?.name ?? name,
    description: meta?.description,
    instructions: body,
  }
}
