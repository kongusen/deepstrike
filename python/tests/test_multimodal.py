from deepstrike._kernel import ContentPartObj, Message
from deepstrike.providers.gemini import GeminiProvider
from deepstrike.providers.base import to_openai_content, to_anthropic_content


def _img_msg() -> Message:
    return Message(
        role="user",
        content="",
        content_parts=[
            ContentPartObj("text", text="What is in this image?"),
            ContentPartObj("image", data="iVBORw0KGgo=", media_type="image/png"),
        ],
    )


def test_gemini_renders_image_inline_data():
    # Gemini used to send only msg.content (text) — images were dropped.
    contents = GeminiProvider("k")._build_contents([_img_msg()])
    parts = contents[0]["parts"]
    assert any(p.get("text") == "What is in this image?" for p in parts)
    img = next(p for p in parts if "inline_data" in p)
    assert img["inline_data"] == {"mime_type": "image/png", "data": "iVBORw0KGgo="}


def test_gemini_renders_url_image_file_data():
    msg = Message(role="user", content="", content_parts=[
        ContentPartObj("image", url="https://x/y.png", media_type="image/png"),
    ])
    parts = GeminiProvider("k")._build_contents([msg])[0]["parts"]
    fd = next(p for p in parts if "file_data" in p)
    assert fd["file_data"] == {"mime_type": "image/png", "file_uri": "https://x/y.png"}


def test_openai_and_anthropic_image_content():
    msg = _img_msg()
    oa = to_openai_content(msg)
    assert any(
        p.get("type") == "image_url" and p["image_url"]["url"] == "data:image/png;base64,iVBORw0KGgo="
        for p in oa
    )
    an = to_anthropic_content(msg)
    assert any(p.get("type") == "image" and p["source"]["data"] == "iVBORw0KGgo=" for p in an)
