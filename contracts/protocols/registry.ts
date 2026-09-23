import type { BoundaryProtocol } from "./types.js"
import { AGENT_PUBLIC_TO_HOST_PROTOCOL } from "./agent-public-to-host.js"
import { CAPABILITY_HOST_TO_KERNEL_PROTOCOL } from "./capability-host-to-kernel.js"
import { CONFIGURE_RUN_HOST_TO_KERNEL_PROTOCOL } from "./configure-run-host-to-kernel.js"
import { KERNEL_PROJECTION_HOST_TO_KERNEL_PROTOCOL } from "./kernel-projection-host-to-kernel.js"
import { MEMORY_HOST_TO_KERNEL_PROTOCOL } from "./memory-host-to-kernel.js"
import { KERNEL_OBSERVATION_TO_SESSION_EVENT_PROTOCOL } from "./kernel-observation-to-session-event.js"
import { PROVIDER_USAGE_DECODE_PROTOCOL } from "./provider-usage-decode.js"
import { PROVIDER_REQUEST_BUILD_PROTOCOL } from "./provider-request-build.js"
import { OPENAI_REQUEST_BUILD_PROTOCOL } from "./openai-request-build.js"
import { PROVIDER_STREAM_DECODE_PROTOCOL } from "./provider-stream-decode.js"
import { PROVIDER_STREAM_FINISH_PROTOCOL } from "./provider-stream-finish.js"
import { SKILL_HOST_TO_KERNEL_PROTOCOL } from "./skill-host-to-kernel.js"
import { SIGNAL_HOST_TO_KERNEL_PROTOCOL } from "./signal-host-to-kernel.js"
import { WORKFLOW_HOST_TO_KERNEL_PROTOCOL } from "./workflow-host-to-kernel.js"

/** Single source of registered boundary protocols. */
export const BOUNDARY_PROTOCOLS: readonly BoundaryProtocol[] = [
  AGENT_PUBLIC_TO_HOST_PROTOCOL,
  CAPABILITY_HOST_TO_KERNEL_PROTOCOL,
  CONFIGURE_RUN_HOST_TO_KERNEL_PROTOCOL,
  KERNEL_PROJECTION_HOST_TO_KERNEL_PROTOCOL,
  MEMORY_HOST_TO_KERNEL_PROTOCOL,
  KERNEL_OBSERVATION_TO_SESSION_EVENT_PROTOCOL,
  PROVIDER_USAGE_DECODE_PROTOCOL,
  PROVIDER_REQUEST_BUILD_PROTOCOL,
  OPENAI_REQUEST_BUILD_PROTOCOL,
  PROVIDER_STREAM_DECODE_PROTOCOL,
  PROVIDER_STREAM_FINISH_PROTOCOL,
  SKILL_HOST_TO_KERNEL_PROTOCOL,
  SIGNAL_HOST_TO_KERNEL_PROTOCOL,
  WORKFLOW_HOST_TO_KERNEL_PROTOCOL,
]
