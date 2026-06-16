/**
 * BenchScenario / BenchVariant — the strategy objects every mechanism plugs into.
 *
 * A scenario is a static recipe (tasks + tools + skills + prompt). A variant is a way to mutate
 * the runtime/scenario before a sample: write skill files differently, overlay RuntimeOptions,
 * inject a different compression policy, etc. The runner iterates variants, calls
 * `variant.setup(scenario, opts)` to get a `VariantSetup`, runs every task once under that setup,
 * collects per-turn metrics + session events, and feeds them to the aggregator.
 *
 * Variants are SCOPED to a scenario — gating-dwell declares "off"/"on", a compression scenario
 * would declare "soft"/"aggressive". The runner accepts variant ids by name; passing an unknown
 * id fails fast at CLI level. No global variant registry.
 *
 * @typedef {Object} BenchTask
 * @property {string} id                    Stable task id (used in session ids).
 * @property {string} goal                  The user goal handed to runner.run.
 * @property {string[]} [criteria]          Optional criteria passed to runner.run.
 *
 * @typedef {Object} BenchSkill
 * @property {string} name                  File-safe skill name.
 * @property {string} description           One-line description for the frontmatter.
 * @property {string} when_to_use           when_to_use frontmatter field.
 * @property {string} body                  Skill body (markdown).
 * @property {string[]} canonicalTools      The tool ids this skill needs — emitted as
 *                                          `allowed_tools` ONLY when the variant asks for it.
 *
 * @typedef {Object} VariantSetup
 * @property {Record<string, any>} [runtimeOverlay]   Merged into RuntimeOptions before runner ctor.
 * @property {() => Promise<void> | void} [cleanup]   Called after the variant's last task finishes
 *                                                    (e.g. rm -rf the skillDir, close handles).
 *
 * @typedef {Object} BenchVariantContext
 * @property {string} variantId             The id this setup is being called for.
 * @property {string} runRoot               Output dir for this run (per-CLI-invocation).
 * @property {string} scenarioId            The scenario being set up.
 *
 * @typedef {Object} BenchVariant
 * @property {string} description           Human-readable variant label.
 * @property {(scenario: BenchScenario, ctx: BenchVariantContext) => VariantSetup | Promise<VariantSetup>} setup
 *                                          Build the setup right before running. Allowed to do IO
 *                                          (mkdtemp, write skill files, etc.).
 *
 * @typedef {Object} BenchScenario
 * @property {string} id                    Stable scenario id (e.g. "gating-dwell").
 * @property {string} description           Human-readable summary.
 * @property {string} systemPrompt          System prompt baked into runner.systemPrompt.
 * @property {BenchTask[]} tasks            Each task = one runner.run call = one session.
 * @property {BenchSkill[]} [skills]        Optional; variants decide how to render them.
 * @property {(sessionId: string) => Promise<any[]> | any[]} mkTools  Tool factory (session-local state allowed; may be async to defer SDK load).
 * @property {number} maxTurns
 * @property {number} maxTokens
 * @property {number} [timeoutMs]
 * @property {Record<string, BenchVariant>} variants
 * @property {string[]} [variantOrder]      Display/iteration order. Defaults to Object.keys(variants).
 * @property {(args: { events: any[], turnMetrics: any[] }) => Record<string, number>} [mechanismHook]
 *                                          Optional: produce mechanism-specific metrics from a single
 *                                          session's events + per-turn metrics. The aggregator merges
 *                                          per-session results across sessions (mean+stdev) into the
 *                                          `mechanism` layer of MetricSet.
 */
export {}
