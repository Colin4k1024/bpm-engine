"""BPM Engine Python Worker SDK.

Provides a pull-based client and worker runtime for external tasks,
mirroring the Rust Worker SDK's functionality.

Usage::

    from bpm_engine_sdk import EngineClient, Worker, TaskHandler, TaskResult

    class MyHandler(TaskHandler):
        @property
        def task_type(self) -> str:
            return "payment"

        async def handle(self, task, ctx):
            # process task
            return TaskResult.complete({"status": "paid"})

    client = EngineClient("http://127.0.0.1:3000")
    worker = Worker(client, [MyHandler()], worker_id="worker-1")
    await worker.start()
"""

from bpm_engine_sdk.client import EngineClient
from bpm_engine_sdk.handler import TaskContext, TaskHandler
from bpm_engine_sdk.models import ExternalTask, TaskResult
from bpm_engine_sdk.worker import Worker

__all__ = [
    "EngineClient",
    "Worker",
    "TaskHandler",
    "TaskContext",
    "ExternalTask",
    "TaskResult",
]
