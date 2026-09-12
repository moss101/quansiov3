"""
Intelligence plane root. Python owns AI/intelligence workloads behind generated typed RPC contracts
(DOSSIER.md §3). Code here may propose plans, tool calls, memory/skill/capability candidates and model
selections; only the trusted Rust runtime validates and commits them. It must never mutate canonical
runtime state, settle an effect, grant a capability or approve an action.
"""
