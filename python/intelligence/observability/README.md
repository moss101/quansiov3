# intelligence.observability

**Canonical owner:** Python intelligence (OPS-003). Each RPC continues the runtime's W3C
`traceparent` as a span of the same trace (`tracing.py`); every log record is one redacted JSON
line carrying the trace ids (`install_json_logging`). Secret canaries and credential shapes are
redacted before anything is written. Never a second log store.
