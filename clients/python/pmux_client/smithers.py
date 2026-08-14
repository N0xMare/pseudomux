"""Small durable-ID helper shared with Smithers-style orchestrators."""

from __future__ import annotations

import uuid

from .protocol import TurnId

SMITHERS_TURN_NAMESPACE = uuid.UUID("7ec46f2d-5f29-5ebc-9ac1-925b0a76f76d")


def turn_id_for_attempt(durable_task_attempt_id: str) -> TurnId:
    """Map one durable attempt ID to a deterministic RFC 4122 UUIDv5 TurnId."""
    if not durable_task_attempt_id:
        raise ValueError("durable_task_attempt_id must not be empty")
    return str(uuid.uuid5(SMITHERS_TURN_NAMESPACE, durable_task_attempt_id))
