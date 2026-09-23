import type { BoundaryProtocol } from "./types.js"
import { CAPABILITY_HOST_TO_KERNEL_PROTOCOL } from "./capability-host-to-kernel.js"
import { CONFIGURE_RUN_HOST_TO_KERNEL_PROTOCOL } from "./configure-run-host-to-kernel.js"
import { SKILL_HOST_TO_KERNEL_PROTOCOL } from "./skill-host-to-kernel.js"

/** Single source of registered boundary protocols. */
export const BOUNDARY_PROTOCOLS: readonly BoundaryProtocol[] = [
  CAPABILITY_HOST_TO_KERNEL_PROTOCOL,
  CONFIGURE_RUN_HOST_TO_KERNEL_PROTOCOL,
  SKILL_HOST_TO_KERNEL_PROTOCOL,
]
