"""Webhook redelivery dedupe (EXEC-011): idempotent and tenant scoped."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field


@dataclass(slots=True)
class WebhookReplay:
    """Deliver each `(tenant_id, event_id)` exactly once, whatever the redeliveries."""

    delivered: set[tuple[str, str]] = field(default_factory=set)
    log: list[tuple[str, str, Mapping[str, object]]] = field(default_factory=list)

    def deliver(
        self,
        *,
        tenant_id: str,
        event_id: str,
        payload: Mapping[str, object],
    ) -> bool:
        """True when this is the first delivery of the event; a replay is a no-op.

        The tenant is part of the key, so another tenant's identical event id is a
        distinct delivery — cross-tenant suppression would be a leak.
        """
        key = (tenant_id, event_id)
        if key in self.delivered:
            return False
        self.delivered.add(key)
        self.log.append((tenant_id, event_id, dict(payload)))
        return True
