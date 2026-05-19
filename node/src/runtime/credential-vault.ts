export interface CredentialVault {
  get(key: string): Promise<string | undefined>
}

export class EnvCredentialVault implements CredentialVault {
  async get(key: string): Promise<string | undefined> {
    return process.env[key]
  }
}

export class InMemoryCredentialVault implements CredentialVault {
  private store: Map<string, string>

  constructor(init?: Record<string, string>) {
    this.store = new Map(Object.entries(init ?? {}))
  }

  set(key: string, value: string): this {
    this.store.set(key, value)
    return this
  }

  async get(key: string): Promise<string | undefined> {
    return this.store.get(key)
  }
}

/** Tries each vault in order, returning the first defined value. */
export class ChainedCredentialVault implements CredentialVault {
  constructor(private readonly vaults: CredentialVault[]) {}

  async get(key: string): Promise<string | undefined> {
    for (const vault of this.vaults) {
      const val = await vault.get(key)
      if (val !== undefined) return val
    }
    return undefined
  }
}
