import { getKernel } from "./kernel.js"

/** cl100k BPE plus the kernel's safety margin; an estimate, not a provider-native count. */
export function estimateTextTokens(text: string): number {
  return getKernel().estimateTextTokens(text)
}
