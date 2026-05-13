import { Governance } from "@deepstrike/sdk"

export function makePolicy() {
  const gov = new Governance("allow")

  // export_dataset writes to disk — require explicit user confirmation
  gov.addPermissionRule("export_dataset", "ask_user")

  // domain blocking is handled inside fetch_and_clip tool itself

  return gov
}
