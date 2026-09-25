// Host-mounted example only. The benchmark reads this file as a lazy resource; it does not execute it.
const REQUIRED = ["situation", "severity", "nextCheckpoint"]
export function validateIncident(value) {
  return REQUIRED.every(key => value && typeof value[key] === "string" && value[key].length > 0)
}
console.log("VALIDATOR_SCRIPT_ORBIT")
