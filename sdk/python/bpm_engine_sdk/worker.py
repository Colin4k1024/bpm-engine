"""Worker runtime: poll loop and task execution.

Mirrors the Rust SDK's Worker struct.
"""

from __future__ import annotations

import asyncio
import logging
import uuid
from typing import Sequence

from bpm_engine_sdk.client import EngineClient, EngineError
from bpm_engine_sdk.handler import TaskContext, TaskHandler
from bpm_engine_sdk.models import ExternalTask

logger = logging.getLogger(__name__)


class Worker:
    """Poll-based worker that fetches and executes external tasks.

    Usage::

        client = EngineClient("http://127.0.0.1:3000")
        worker = Worker(
            client,
            [PaymentHandler()],
            worker_id="worker-1",
            poll_interval=1.0,
        )
        await worker.start()
    """

    def __init__(
        self,
        client: EngineClient,
        handlers: Sequence[TaskHandler],
        *,
        worker_id: str | None = None,
        max_tasks: int = 10,
        lock_duration_ms: int = 30_000,
        poll_interval: float = 1.0,
        fetch_retry_max: int = 5,
        fetch_retry_backoff: float = 1.0,
    ) -> None:
        self._client = client
        self._handlers: dict[str, TaskHandler] = {h.task_type: h for h in handlers}
        self._worker_id = worker_id or f"worker-{uuid.uuid4().hex[:8]}"
        self._max_tasks = max_tasks
        self._lock_duration_ms = lock_duration_ms
        self._poll_interval = poll_interval
        self._fetch_retry_max = fetch_retry_max
        self._fetch_retry_backoff = fetch_retry_backoff
        self._running = False

    @property
    def worker_id(self) -> str:
        return self._worker_id

    async def start(self) -> None:
        """Run the poll loop until cancelled.

        Fetches tasks, dispatches them to handlers, and reports results.
        Implements exponential backoff on fetch errors.
        """
        self._running = True
        task_types = list(self._handlers.keys())
        if not task_types:
            logger.warning("no handlers registered; worker will not fetch any tasks")

        while self._running:
            if not task_types:
                await asyncio.sleep(self._poll_interval)
                continue

            backoff = self._fetch_retry_backoff
            for attempt in range(self._fetch_retry_max + 1):
                try:
                    tasks = await self._client.fetch_and_lock(
                        worker_id=self._worker_id,
                        task_types=task_types,
                        max_tasks=self._max_tasks,
                        lock_duration_ms=self._lock_duration_ms,
                    )
                    for task in tasks:
                        if task.task_type in self._handlers:
                            asyncio.create_task(self._execute_task(task))
                        else:
                            logger.warning("no handler for task type: %s", task.task_type)
                    break
                except EngineError as e:
                    if attempt < self._fetch_retry_max:
                        logger.warning(
                            "fetch_and_lock failed (attempt %d/%d), retrying in %.1fs: %s",
                            attempt + 1,
                            self._fetch_retry_max,
                            backoff,
                            e,
                        )
                        await asyncio.sleep(backoff)
                        backoff = min(backoff * 2, 30.0)
                    else:
                        logger.error("fetch_and_lock failed after %d retries: %s", self._fetch_retry_max, e)
                except Exception as e:
                    if attempt < self._fetch_retry_max:
                        logger.warning(
                            "fetch_and_lock error (attempt %d/%d), retrying in %.1fs: %s",
                            attempt + 1,
                            self._fetch_retry_max,
                            backoff,
                            e,
                        )
                        await asyncio.sleep(backoff)
                        backoff = min(backoff * 2, 30.0)
                    else:
                        logger.error("fetch_and_lock failed after retries: %s", e)

            await asyncio.sleep(self._poll_interval)

    async def stop(self) -> None:
        """Signal the worker to stop after the current poll cycle."""
        self._running = False

    async def _execute_task(self, task: ExternalTask) -> None:
        """Execute a single task with its handler."""
        handler = self._handlers.get(task.task_type)
        if not handler:
            return

        ctx = TaskContext(worker_id=self._worker_id, task_id=task.task_id, _client=self._client)
        try:
            result = await handler.handle(task, ctx)
            if result.status == "complete":
                logger.info(
                    "task %s completed by worker %s",
                    task.task_id,
                    self._worker_id,
                )
                await self._client.complete(
                    task.task_id, self._worker_id, result.variables
                )
            else:
                logger.warning(
                    "task %s failed by worker %s: %s",
                    task.task_id,
                    self._worker_id,
                    result.error,
                )
                await self._client.fail(
                    task.task_id,
                    self._worker_id,
                    result.error,
                    result.retry_after_ms,
                )
        except Exception as e:
            logger.exception("handler panic for task %s", task.task_id)
            try:
                await self._client.fail(
                    task.task_id, self._worker_id, f"handler error: {e}"
                )
            except Exception:
                logger.exception("failed to report task failure for %s", task.task_id)
