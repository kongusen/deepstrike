import pytest

from deepstrike.providers.openai import OpenAIProvider
from deepstrike.providers.gemini import GeminiProvider
from deepstrike.providers.ollama import OllamaProvider
from deepstrike.providers.base import RenderedContext
from deepstrike._kernel import Message
from deepstrike.providers.stream import ToolCallEvent


@pytest.mark.asyncio
async def test_openai_flushes_tool_calls_when_stream_ends_without_tool_finish_reason(monkeypatch):
    provider = OpenAIProvider("test-key")

    class FakeCompletions:
        async def create(self, **kwargs):
            async def gen():
                class Fn: pass
                class TC: pass
                class Delta: pass
                class Choice: pass
                class Chunk: pass
                fn1 = Fn(); fn1.name = "look"; fn1.arguments = '{"q":'
                tc1 = TC(); tc1.index = 0; tc1.id = "call_1"; tc1.function = fn1
                d1 = Delta(); d1.content = None; d1.tool_calls = [tc1]
                c1 = Choice(); c1.delta = d1; c1.finish_reason = None
                ch1 = Chunk(); ch1.choices = [c1]; ch1.usage = None
                yield ch1
                fn2 = Fn(); fn2.name = "up"; fn2.arguments = '"x"}'
                tc2 = TC(); tc2.index = 0; tc2.id = None; tc2.function = fn2
                d2 = Delta(); d2.content = None; d2.tool_calls = [tc2]
                c2 = Choice(); c2.delta = d2; c2.finish_reason = "stop"
                ch2 = Chunk(); ch2.choices = [c2]; ch2.usage = None
                yield ch2
            return gen()

    class FakeChat: completions = FakeCompletions()
    class FakeClient: chat = FakeChat()
    provider._client = FakeClient()

    gen = provider.stream(RenderedContext(turns=[Message(role="user", content="hi")]), [])
    events = [event async for event in gen]
    assert any(isinstance(e, ToolCallEvent) and e.name == "lookup" and e.arguments == {"q": "x"} for e in events)


@pytest.mark.asyncio
async def test_gemini_keeps_duplicate_function_names_distinct(monkeypatch):
    provider = GeminiProvider("test-key")

    class FunctionCall:
        def __init__(self, name, args): self.name = name; self.args = args
    class Part:
        def __init__(self, fc): self.function_call = fc
    class Content:
        def __init__(self, parts): self.parts = parts
    class Candidate:
        def __init__(self, parts): self.content = Content(parts)
    class Chunk:
        def __init__(self, parts): self.candidates = [Candidate(parts)]
    class Response:
        usage_metadata = None
    class FakeModel:
        async def generate_content_async(self, contents, stream=False):
            async def chunks():
                yield Chunk([Part(FunctionCall("lookup", {"q": "a"}))])
                yield Chunk([Part(FunctionCall("lookup", {"q": "b"}))])
            return chunks()
    provider._model = FakeModel()

    gen = provider.stream(RenderedContext(turns=[Message(role="user", content="hi")]), [])
    events = [event async for event in gen]
    tool_events = [e for e in events if isinstance(e, ToolCallEvent)]
    assert [(e.id, e.arguments) for e in tool_events] == [("call_1", {"q": "a"}), ("call_2", {"q": "b"})]
