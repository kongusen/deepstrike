import { createTextKnowledgeSource } from "../src/knowledge/source.js"

test("text knowledge source retrieves matching documents on demand", async () => {
  const source = createTextKnowledgeSource([
    { id: "billing", name: "Billing", content: "Invoices are issued monthly." },
    { id: "security", name: "Security", content: "Rotate credentials every quarter." },
  ])
  await source.init()
  await expect(source.retrieve("monthly invoices")).resolves.toEqual(["[Knowledge: Billing]\nInvoices are issued monthly."])
  await expect(source.retrieve("unrelated topic")).resolves.toEqual([])
})
