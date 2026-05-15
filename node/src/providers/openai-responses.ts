import OpenAI from "openai"
import type { Message, ProviderRunState, StreamEvent, TextDelta, ToolCallEvent, ToolSchema, LLMProvider } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker } from "./base.js"
import { normalizeToolCall } from "./base.js"

export interface OpenAIResponsesRunState extends ProviderRunState {
  previousResponseId?: string
  coveredMessageCount: number
}

export class OpenAIResponsesAdapter {
  buildTools(tools: ToolSchema[]) {
    return tools.map(t => ({
      type: "function" as const,
      name: t.name,
      description: t.description,
      parameters: JSON.parse(t.parameters),
    }))
  }

  buildInstructions(messages: Message[]): string | undefined {
    const instructions = messages
      .filter(message => message.role === "system")
      .map(message => message.content)
      .filter(Boolean)

    return instructions.length ? instructions.join("\n\n") : undefined
  }

  buildInput(messages: Message[], state?: OpenAIResponsesRunState): Array<Record<string, unknown>> {
    const input: Array<Record<string, unknown>> = []
    const uncoveredMessages = state?.previousResponseId
      ? messages.slice(state.coveredMessageCount)
      : messages

    for (const message of uncoveredMessages) {
      if (message.role === "system") {
        continue
      }

      if (message.role === "assistant" && message.toolCalls?.length) {
        if (message.content || message.contentParts?.length) {
          input.push({
            role: "assistant",
            content: this.buildMessageContent(message),
          })
        }
        for (const tc of message.toolCalls) {
          input.push({
            type: "function_call",
            call_id: tc.id,
            name: tc.name,
            arguments: tc.arguments,
          })
        }
        continue
      }

      if (message.role === "tool") {
        for (const part of message.contentParts ?? []) {
          if (part.type !== "tool_result") continue
          input.push({
            type: "function_call_output",
            call_id: part.callId,
            output: part.output,
          })
        }
        continue
      }

      input.push({
        role: message.role,
        content: this.buildMessageContent(message),
      })
    }

    return input
  }

  decodeOutput(output: Array<Record<string, unknown>>): {
    content: string
    toolCalls: Array<{ id: string; name: string; arguments: string }>
  } {
    let content = ""
    const toolCalls: Array<{ id: string; name: string; arguments: string }> = []

    for (const item of output) {
      if (item.type === "message") {
        for (const part of (item.content as Array<Record<string, unknown>> | undefined) ?? []) {
          if (part.type === "output_text") content += String(part.text ?? "")
        }
      } else if (item.type === "function_call") {
        const toolCall = normalizeToolCall(
          String(item.call_id ?? item.id ?? ""),
          String(item.name ?? ""),
          item.arguments ?? "{}",
        )
        if (toolCall) toolCalls.push(toolCall)
      }
    }

    return { content, toolCalls }
  }

  private buildMessageContent(message: Message): string | Array<Record<string, unknown>> {
    if (!message.contentParts?.length) return message.content

    const content: Array<Record<string, unknown>> = []
    for (const part of message.contentParts) {
      if (part.type === "text") {
        content.push({ type: "input_text", text: part.text })
        continue
      }
      if (part.type === "image") {
        const imageUrl = part.url ?? (
          part.data && part.mediaType
            ? `data:${part.mediaType};base64,${part.data}`
            : undefined
        )
        if (imageUrl) content.push({
          type: "input_image",
          detail: part.detail ?? "auto",
          image_url: imageUrl,
        })
      }
    }

    return content
  }
}

export class OpenAIResponsesProvider implements LLMProvider {
  protected client: OpenAI
  protected circuit: CircuitBreaker
  protected maxRetries: number
  protected baseDelay: number
  protected readonly responses = new OpenAIResponsesAdapter()

  constructor(
    apiKey: string,
    protected readonly model = "gpt-4.1",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL = "https://api.openai.com/v1",
  ) {
    this.client = withServerRuntimeGuard(() => new OpenAI({ apiKey, baseURL }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
  }

  createRunState(): OpenAIResponsesRunState {
    return { coveredMessageCount: 0 }
  }

  async complete(messages: Message[], tools: ToolSchema[]): Promise<Message> {
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    let lastErr: unknown

    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const instructions = this.responses.buildInstructions(messages)
        const resp = await this.client.responses.create({
          model: this.model,
          input: this.responses.buildInput(messages) as unknown as OpenAI.Responses.ResponseInput,
          ...(instructions ? { instructions } : {}),
          ...(tools.length ? { tools: this.responses.buildTools(tools) as OpenAI.Responses.Tool[] } : {}),
        })
        this.circuit.recordSuccess()
        const decoded = this.responses.decodeOutput(resp.output as unknown as Array<Record<string, unknown>>)
        return {
          role: "assistant",
          content: decoded.content,
          toolCalls: decoded.toolCalls,
          tokenCount: resp.usage?.total_tokens,
        }
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }

    throw lastErr
  }

  async *stream(
    messages: Message[],
    tools: ToolSchema[],
    _extensions?: Record<string, unknown>,
    state?: ProviderRunState,
  ): AsyncIterable<StreamEvent> {
    const runState = this.asRunState(state)
    const functionCalls = new Map<number, { id: string; name: string; argsBuf: string }>()
    const instructions = this.responses.buildInstructions(messages)

    const stream = await this.client.responses.create({
      model: this.model,
      input: this.responses.buildInput(messages, runState) as unknown as OpenAI.Responses.ResponseInput,
      ...(instructions ? { instructions } : {}),
      ...(runState.previousResponseId ? { previous_response_id: runState.previousResponseId } : {}),
      ...(tools.length ? { tools: this.responses.buildTools(tools) as OpenAI.Responses.Tool[] } : {}),
      stream: true,
    })

    for await (const evt of stream) {
      if (evt.type === "response.output_text.delta") {
        yield { type: "text_delta", delta: evt.delta } as TextDelta
      } else if (evt.type === "response.output_item.added" && evt.item.type === "function_call") {
        functionCalls.set(evt.output_index, {
          id: evt.item.call_id,
          name: evt.item.name,
          argsBuf: evt.item.arguments ?? "",
        })
      } else if (evt.type === "response.function_call_arguments.delta") {
        const call = functionCalls.get(evt.output_index)
        if (call) call.argsBuf += evt.delta
      } else if (evt.type === "response.function_call_arguments.done") {
        const call = functionCalls.get(evt.output_index)
        if (call) call.argsBuf = evt.arguments
      } else if (evt.type === "response.output_item.done" && evt.item.type === "function_call") {
        const call = functionCalls.get(evt.output_index) ?? {
          id: evt.item.call_id,
          name: evt.item.name,
          argsBuf: evt.item.arguments ?? "{}",
        }
        let args: Record<string, unknown> = {}
        try { args = JSON.parse(call.argsBuf || "{}") } catch { args = {} }
        yield { type: "tool_call", id: call.id, name: call.name, arguments: args } as ToolCallEvent
      } else if (evt.type === "response.completed") {
        runState.previousResponseId = evt.response.id
        runState.coveredMessageCount = messages.length + 1
        if (evt.response.usage?.total_tokens) {
          yield { type: "usage", totalTokens: evt.response.usage.total_tokens } as StreamEvent
        }
      }
    }
  }

  private asRunState(state?: ProviderRunState): OpenAIResponsesRunState {
    if (!state) return this.createRunState()
    if (typeof state.coveredMessageCount !== "number") state.coveredMessageCount = 0
    return state as OpenAIResponsesRunState
  }
}
