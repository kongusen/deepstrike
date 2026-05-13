import { dirname, resolve } from "path"
import { fileURLToPath } from "url"

const __dirname = dirname(fileURLToPath(import.meta.url))

export const ROOT = resolve(__dirname, "..")
export const SKILLS_DIR = resolve(ROOT, "skills")
export const INBOX_DIR = resolve(ROOT, "inbox")
export const ARCHIVE_DIR = resolve(ROOT, "archive")
export const OUTPUT_DIR = resolve(ROOT, "output")
export const MEMORY_DIR = resolve(OUTPUT_DIR, "memory")
