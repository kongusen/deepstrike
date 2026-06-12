from importlib.metadata import PackageNotFoundError, version

try:
    __version__ = version("deepstrike")
except PackageNotFoundError:
    __version__ = "0+unknown"

from deepstrike._kernel import (
    Message, ToolCall, ToolResult, ToolSchema,
    SkillMetadata,
)
from deepstrike.runtime import (
    RuntimeRunner,
    RuntimeOptions,
    ResourceQuota,
    MemoryPolicy,
    MemoryWriteRateLimit,
    SchedulerBudget,
    SubAgentHarnessConfig,
    OsProfile,
    assert_native_profile,
    os_profile,
    collect_text,
    LocalExecutionPlane,
    InMemorySessionLog,
    FileSessionLog,
    SessionLog,
    ProviderReplay,
    FilteredExecutionPlane,
    SubAgentOrchestrator,
    spawn_standalone,
    default_sub_agent_orchestrator,
    DEFAULT_SANDBOX_POLICY,
    validate_declarative_policy,
)
from deepstrike.governance import Governance, GovernanceVerdict
from deepstrike.providers import (
    LLMProvider, RenderedContext, ProviderRunState, RuntimePolicy,
    AnthropicProvider, OpenAIProvider,
    QwenProvider, DeepSeekProvider, MiniMaxAnthropicProvider, MiniMaxOpenAIProvider, OllamaProvider, KimiProvider,
    StreamEvent, TextDelta, ThinkingDelta,
    ToolCallEvent, ToolDeltaEvent, ToolSuspendEvent, ToolResultEvent, DoneEvent, ErrorEvent,
    PermissionRequestEvent, PermissionResolvedEvent, PermissionResponse, ToolArgumentRepairedEvent,
    RetryConfig, CircuitBreaker, TokenUsage, ProviderToolSpec,
)
from deepstrike.tools import RegisteredTool, tool, streaming_tool, validate_tool_arguments, execute_tools, read_file
from deepstrike.memory import (
    WorkingMemory,
    DreamStore, DreamResult, SessionData, MemoryEntry, CurationResult, CurationStats,
)
from deepstrike.safety import PermissionManager, PermissionMode, Permission, PermissionDecision
from deepstrike.harness import (
    QualityGate,
    SinglePassHarness, HarnessLoop, EvalLoopHarness,
    HarnessRequest, HarnessOutcome,
)
from deepstrike.skills import SkillRegistry
from deepstrike.knowledge import KnowledgeSource
from deepstrike.signals import RuntimeSignal, SignalSource, ScheduledPrompt, SignalGateway
from deepstrike.types.agent import (
    AgentIdentity, AgentCapabilityFilter, AgentRunSpec,
    AgentProcessChangedObservation, SubAgentResult, LoopResult,
    KernelAgentRole, AgentIsolation, ContextInheritance,
    MilestoneContract, MilestonePhase, MilestoneCheckResult, MilestonePolicy,
    milestone_check_pass, milestone_check_fail,
    WorkflowSpec, WorkflowNodeSpec, WorkflowSpawnInfo, workflow_spec_to_kernel,
    workflow_node_spec_to_kernel, submit_workflow_nodes_to_kernel, submit_workflow_nodes_tool,
    fanout_synthesize, generate_and_filter, verify_rules,
)
from deepstrike.collaboration import (
    AcceptanceCriterion, VerificationContract, ContractCheckResult,
    ContractBuilder, format_contract_for_system_prompt, contract_to_criteria_strings,
    AgentPool, AgentRole, IsolatedVerifierContext,
    ContractDrivenHarness, ContractOutcome, ContractHarnessOptions, Violation,
    HandoffArtifact, HandoffBus, ContractOutcomeInput,
    CreatorVerifierMode, OrchestrationMode, CreatorVerifierMetrics,
)
__all__ = [
    "RuntimeRunner",
    "RuntimeOptions",
    "ResourceQuota",
    "MemoryPolicy",
    "MemoryWriteRateLimit",
    "SchedulerBudget",
    "SubAgentHarnessConfig",
    "OsProfile",
    "assert_native_profile",
    "os_profile",
    "collect_text",
    "LocalExecutionPlane",
    "InMemorySessionLog",
    "FileSessionLog",
    "SessionLog",
    "ProviderReplay",
    "ProviderReplay",
    "ProviderReplay",
    "LLMProvider", "RenderedContext", "ProviderRunState", "RuntimePolicy", "AnthropicProvider", "OpenAIProvider",
    "QwenProvider", "DeepSeekProvider", "MiniMaxAnthropicProvider", "MiniMaxOpenAIProvider", "OllamaProvider", "KimiProvider",
    "StreamEvent", "TextDelta", "ThinkingDelta",
    "ToolCallEvent", "ToolDeltaEvent", "ToolSuspendEvent", "ToolResultEvent", "DoneEvent", "ErrorEvent",
    "PermissionRequestEvent", "PermissionResolvedEvent", "PermissionResponse", "ToolArgumentRepairedEvent",
    "RetryConfig", "CircuitBreaker", "TokenUsage", "ProviderToolSpec",
    "RegisteredTool", "tool", "streaming_tool", "validate_tool_arguments", "execute_tools", "read_file",
    "WorkingMemory",
    "DreamStore", "DreamResult", "SessionData", "MemoryEntry", "CurationResult", "CurationStats",
    "PermissionManager", "PermissionMode", "Permission", "PermissionDecision",
    "QualityGate",
    "SinglePassHarness", "HarnessLoop", "EvalLoopHarness", "HarnessRequest", "HarnessOutcome",
    "SkillRegistry",
    "KnowledgeSource",
    "RuntimeSignal", "SignalSource", "ScheduledPrompt", "SignalGateway",
    "Message", "ToolCall", "ToolResult", "ToolSchema",
    "SkillMetadata",
    "Governance", "GovernanceVerdict",
    # Sub-agent isolation
    "AgentIdentity", "AgentCapabilityFilter", "AgentRunSpec",
    "AgentProcessChangedObservation", "SubAgentResult", "LoopResult",
    "KernelAgentRole", "AgentIsolation", "ContextInheritance",
    "MilestoneContract", "MilestonePhase", "MilestoneCheckResult", "MilestonePolicy",
    "milestone_check_pass", "milestone_check_fail",
    "FilteredExecutionPlane",
    "SubAgentOrchestrator", "spawn_standalone", "default_sub_agent_orchestrator",
    "DEFAULT_SANDBOX_POLICY", "validate_declarative_policy",
    # Collaboration layer
    "AcceptanceCriterion", "VerificationContract", "ContractCheckResult",
    "ContractBuilder", "format_contract_for_system_prompt", "contract_to_criteria_strings",
    "AgentPool", "AgentRole", "IsolatedVerifierContext",
    "ContractDrivenHarness", "ContractOutcome", "ContractHarnessOptions", "Violation",
    "HandoffArtifact", "HandoffBus", "ContractOutcomeInput",
    "CreatorVerifierMode", "OrchestrationMode", "CreatorVerifierMetrics",
    # Workflow DAG drive (W0-ABI)
    "WorkflowSpec", "WorkflowNodeSpec", "WorkflowSpawnInfo", "workflow_spec_to_kernel",
    "workflow_node_spec_to_kernel", "submit_workflow_nodes_to_kernel", "submit_workflow_nodes_tool",
    "fanout_synthesize", "generate_and_filter", "verify_rules",
    "__version__",
]
