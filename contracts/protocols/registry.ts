import type { BoundaryProtocol } from "./types.js"
import { SKILL_HOST_TO_KERNEL_PROTOCOL } from "./skill-host-to-kernel.js"

/** Single source of registered boundary protocols. */
export const BOUNDARY_PROTOCOLS: readonly BoundaryProtocol[] = [
  SKILL_HOST_TO_KERNEL_PROTOCOL,
]
