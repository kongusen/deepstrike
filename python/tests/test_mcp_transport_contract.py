import pytest

from deepstrike._kernel import ToolCall, ToolSchema
from deepstrike.runtime.credential_vault import InMemoryCredentialVault
from deepstrike.runtime.mcp_proxy_plane import McpProxyPlane, McpServerConfig


class FakeConnection:
    def __init__(self, name, config, vault):
        self.schema = ToolSchema(name="remote_tool", description="remote", parameters='{"type":"object"}')

    async def start(self):
        return None

    def schemas(self):
        return [self.schema]

    async def execute(self, call):
        return "ok", False, None

    async def stop(self):
        return None


@pytest.mark.asyncio
async def test_custom_transport_factory_can_handle_non_stdio():
    plane = McpProxyPlane(
        servers={"remote": McpServerConfig(command="unused", transport="http")},
        vault=InMemoryCredentialVault(),
        connection_factory=FakeConnection,
    )
    await plane.connect()
    assert [schema.name for schema in plane.schemas()] == ["remote_tool"]
    await plane.disconnect()


@pytest.mark.asyncio
async def test_default_non_stdio_transport_remains_explicitly_unsupported():
    plane = McpProxyPlane(
        servers={"remote": McpServerConfig(command="unused", transport="sse")},
        vault=InMemoryCredentialVault(),
    )
    with pytest.raises(NotImplementedError, match="supported transports: stdio"):
        await plane.connect()
