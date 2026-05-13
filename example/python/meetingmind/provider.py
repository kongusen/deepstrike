import os
from deepstrike import OpenAIProvider


def make_provider() -> OpenAIProvider:
    return OpenAIProvider(
        api_key=os.environ["OPENAI_API_KEY"],
        model=os.environ.get("MODEL", "gpt-4o"),
        base_url=os.environ.get("OPENAI_BASE_URL", "https://api.openai.com/v1"),
    )
