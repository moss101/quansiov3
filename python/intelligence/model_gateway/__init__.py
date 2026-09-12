"""
Model gateway: provider adapters, normalized ModelEvent streaming, model policy/DLP and usage
accounting. Canonical owner: `python/intelligence/model_gateway` (INT-002, INT-003). The only place
provider SDKs may be imported; provider credentials never leave it, and the gateway never executes
tools, commits WorkGraph success or mutates external systems.
"""
