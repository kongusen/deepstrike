import type { BoundaryProtocol } from "./types.js"
import { AGENT_PUBLIC_TO_HOST_PROTOCOL } from "./agent-public-to-host.js"
import { CAPABILITY_HOST_TO_KERNEL_PROTOCOL } from "./capability-host-to-kernel.js"
import { CONFIGURE_RUN_HOST_TO_KERNEL_PROTOCOL } from "./configure-run-host-to-kernel.js"
import { SKILL_HOST_TO_KERNEL_PROTOCOL } from "./skill-host-to-kernel.js"

/** Single source of registered boundary protocols. */
export const BOUNDARY_PROTOCOLS: readonly BoundaryProtocol[] = [
  AGENT_PUBLIC_TO_HOST_PROTOCOL,
  CAPABILITY_HOST_TO_KERNEL_PROTOCOL,
  CONFIGURE_RUN_HOST_TO_KERNEL_PROTOCOL,
  SKILL_HOST_TO_KERNEL_PROTOCOL,
]
