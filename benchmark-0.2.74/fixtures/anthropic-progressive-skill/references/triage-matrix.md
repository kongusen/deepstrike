# Incident triage matrix

Reference marker: `TRIAGE_REFERENCE_ORBIT`.

| Signal | Severity | Required owner | Checkpoint |
| --- | --- | --- | --- |
| One tenant, no data loss, workaround exists | SEV-3 | Service owner | 30 minutes |
| Multiple tenants or elevated error rate | SEV-2 | Service owner + incident commander | 15 minutes |
| Broad outage, security exposure, or data integrity risk | SEV-1 | Incident commander + security lead | 5 minutes |

Use the highest supported severity while evidence is incomplete. Downgrade only after a checkpoint records the signal that disproves the higher class.
