"""TaskHandler abstract base class and TaskContext."""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass
from typing import TYPE_CHECKING

from bpm_engine_sdk.models import ExternalTask, TaskResult

if TYPE_CHECKING:
    from bpm_engine_sdk.client import EngineClient


@dataclass
class TaskContext:
    """Context passed to a task handler during execution.

    Provides access to task metadata and the ability to extend the lock
    for long-running tasks.
    """

    worker_id: str
    task_id: str
    _client: EngineClient | None = None

    async def extend_lock(self, extension_ms: int) -> None:
        """Extend the lock on this task to prevent timeout.

        Call this periodically for tasks that may exceed the initial
        lock duration.

        Args:
            extension_ms: Extension duration in milliseconds.

        Raises:
            RuntimeError: If no client was configured.
            EngineError: If the engine rejects the request.
        """
        if self._client is None:
            raise RuntimeError("extend_lock requires an EngineClient")
        await self._client.extend_lock(self.task_id, self.worker_id, extension_ms)


class TaskHandler(ABC):
    """Abstract base class for external task handlers.

    Implement ``task_type`` and ``handle`` to process tasks of a specific type.

    Example::

        class PaymentHandler(TaskHandler):
            @property
            def task_type(self) -> str:
                return "payment"

            async def handle(self, task: ExternalTask, ctx: TaskContext) -> TaskResult:
                amount = task.variables.get("amount", "0")
                # process payment ...
                return TaskResult.complete({"status": "paid"})
    """

    @property
    @abstractmethod
    def task_type(self) -> str:
        """The task type this handler processes (matches the BPMN topic)."""
        ...

    @abstractmethod
    async def handle(self, task: ExternalTask, ctx: TaskContext) -> TaskResult:
        """Handle an external task and return the result."""
        ...
