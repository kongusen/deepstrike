from __future__ import annotations

import os
from typing import Protocol, runtime_checkable


@runtime_checkable
class CredentialVault(Protocol):
  async def get(self, key: str) -> str | None: ...


class EnvCredentialVault:
  async def get(self, key: str) -> str | None:
    return os.environ.get(key)


class InMemoryCredentialVault:
  def __init__(self, init: dict[str, str] | None = None) -> None:
    self._store: dict[str, str] = dict(init or {})

  def set(self, key: str, value: str) -> "InMemoryCredentialVault":
    self._store[key] = value
    return self

  async def get(self, key: str) -> str | None:
    return self._store.get(key)


class ChainedCredentialVault:
  """Tries each vault in order, returning the first defined value."""

  def __init__(self, *vaults: CredentialVault) -> None:
    self._vaults = vaults

  async def get(self, key: str) -> str | None:
    for vault in self._vaults:
      val = await vault.get(key)
      if val is not None:
        return val
    return None
