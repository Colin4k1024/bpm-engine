"""Data models for the BPM Engine Python SDK."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class ExternalTask:
    """An external task fetched from the engine.

    Mirrors the Rust SDK's ExternalTask struct.
    """

    task_id: str
    task_type: str
    variables: dict[str, str] = field(default_factory=dict)
    lock_expire_at: str | None = None
    retries: int = 0


@dataclass
class TaskResult:
    """Result of handling an external task.

    Use the class methods ``complete()`` and ``fail()`` to create instances.
    """

    status: str  # "complete" or "fail"
    variables: dict[str, str] = field(default_factory=dict)
    error: str = ""
    retry_after_ms: int | None = None

    @classmethod
    def complete(cls, variables: dict[str, str] | None = None) -> TaskResult:
        """Mark the task as completed with optional output variables."""
        return cls(status="complete", variables=variables or {})

    @classmethod
    def fail(
        cls,
        error: str,
        retry_after_ms: int | None = None,
    ) -> TaskResult:
        """Mark the task as failed with an error message and optional retry delay."""
        return cls(status="fail", error=error, retry_after_ms=retry_after_ms)
