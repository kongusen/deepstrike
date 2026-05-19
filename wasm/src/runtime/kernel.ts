type WasmKernel = typeof import("@deepstrike/wasm-kernel")

let kernelMod: WasmKernel | null = null

/** Lazily load the WASM kernel (browser / worker safe). */
export async function getKernel(): Promise<WasmKernel> {
  if (!kernelMod) {
    kernelMod = await import("@deepstrike/wasm-kernel")
  }
  return kernelMod
}
