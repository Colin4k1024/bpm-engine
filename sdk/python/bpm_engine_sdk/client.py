"""HTTP client for the BPM Engine external-task REST API.

Mirrors the Rust SDK's EngineClient.
"""

from __future__ import annotations

import logging
from typing import Any

import httpx

from bpm_engine_sdk.models import ExternalTask

logger = logging.getLogger(__name__)


class EngineError(Exception):
    """Error returned by the BPM Engine REST API."""

    def __init__(self, status: int, message: str) -> None:
        self.status = status
        self.message = message
        super().__init__(f"Engine error (HTTP {status}): {message}")


class EngineClient:
    """HTTP client for Engine external-task endpoints.

    Usage::

        client = EngineClient("http://127.0.0.1:3000")
        tasks = await client.fetch_and_lock("worker-1", ["payment"], max_tasks=5)
    """

    def __init__(
        self,
        base_url: str,
        *,
        tenant_id: str | None = None,
        timeout: float = 30.0,
    ) -> None:
        self._base_url = base_url.rstrip("/")
        self._tenant_id = tenant_id
        self._client = httpx.AsyncClient(timeout=timeout)

    def _url(self, path: str) -> str:
        return f"{self._base_url}/api/v1/external-tasks{path}"

    def _headers(self) -> dict[str, str]:
        headers: dict[str, str] = {}
        if self._tenant_id:
            headers["x-tenant-id"] = self._tenant_id
        return headers

    async def fetch_and_lock(
        self,
        worker_id: str,
        task_types: list[str],
        max_tasks: int = 10,
        lock_duration_ms: int = 30_000,
    ) -> list[ExternalTask]:
        """Fetch and lock tasks from the engine.

        Args:
            worker_id: Unique identifier for this worker.
            task_types: List of task types (topics) to fetch.
            max_tasks: Maximum number of tasks to fetch per call.
            lock_duration_ms: Lock duration in milliseconds.

        Returns:
            List of locked external tasks.
        """
        url = self._url("/fetch-and-lock")
        body = {
            "worker_id": worker_id,
            "task_types": task_types,
            "max_tasks": max_tasks,
            "lock_duration_ms": lock_duration_ms,
        }
        logger.debug("fetch_and_lock: %s", url)
        resp = await self._client.post(url, json=body, headers=self._headers())
        if resp.status_code != 200:
            error_body = resp.json()
            raise EngineError(resp.status_code, error_body.get("error", resp.text))
        items = resp.json()
        return [
            ExternalTask(
                task_id=item["task_id"],
                task_type=item["task_type"],
                variables=item.get("variables", {}),
            )
            for item in items
        ]

    async def complete(
        self,
        task_id: str,
        worker_id: str,
        variables: dict[str, str] | None = None,
    ) -> None:
        """Complete a locked task.

        Args:
            task_id: The task ID to complete.
            worker_id: The worker that owns the lock.
            variables: Optional output variables to merge into the process instance.
        """
        url = self._url(f"/{task_id}/complete")
        body: dict[str, Any] = {"worker_id": worker_id}
        if variables:
            body["variables"] = variables
        logger.debug("complete: %s", url)
        resp = await self._client.post(url, json=body, headers=self._headers())
        if resp.status_code != 200:
            error_body = resp.json()
            raise EngineError(resp.status_code, error_body.get("error", resp.text))

    async def fail(
        self,
        task_id: str,
        worker_id: str,
        error: str,
        retry_after_ms: int | None = None,
    ) -> None:
        """Mark a task as failed.

        Args:
            task_id: The task ID to fail.
            worker_id: The worker that owns the lock.
            error: Error message describing the failure.
            retry_after_ms: Optional delay before the task becomes available again.
        """
        url = self._url(f"/{task_id}/fail")
        body: dict[str, Any] = {
            "worker_id": worker_id,
            "error": error,
        }
        if retry_after_ms is not None:
            body["retry_after_ms"] = retry_after_ms
        logger.debug("fail: %s", url)
        resp = await self._client.post(url, json=body, headers=self._headers())
        if resp.status_code != 200:
            error_body = resp.json()
            raise EngineError(resp.status_code, error_body.get("error", resp.text))

    async def extend_lock(
        self,
        task_id: str,
        worker_id: str,
        extension_ms: int,
    ) -> None:
        """Extend the lock on a locked task.

        Call this periodically for long-running tasks to prevent the lock
        from expiring before processing completes.

        Args:
            task_id: The task ID to extend the lock on.
            worker_id: The worker that owns the lock.
            extension_ms: Extension duration in milliseconds.
        """
        url = self._url(f"/{task_id}/extend-lock")
        body = {
            "worker_id": worker_id,
            "extension_ms": extension_ms,
        }
        logger.debug("extend_lock: %s", url)
        resp = await self._client.post(url, json=body, headers=self._headers())
        if resp.status_code != 200:
            error_body = resp.json()
            raise EngineError(resp.status_code, error_body.get("error", resp.text))

    async def close(self) -> None:
        """Close the underlying HTTP client."""
        await self._client.aclose()
