#!/usr/bin/env python3
"""Assert the externally observable A2A worker-progress contract."""

from __future__ import annotations

import os
import re
import time
import uuid
from typing import Any

from inkbox import Inkbox

STOPPED_STATES = {
    "TASK_STATE_COMPLETED",
    "TASK_STATE_FAILED",
    "TASK_STATE_CANCELED",
    "TASK_STATE_REJECTED",
    "TASK_STATE_INPUT_REQUIRED",
    "TASK_STATE_AUTH_REQUIRED",
}
ACK_SUFFIX = "Expect progress updates about every 1 minute."
PROGRESS = re.compile(
    r"^I'm working through the requested calculation\. \((\d+)s elapsed\)$"
)


def required_env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise RuntimeError(f"{name} is required")
    return value


def enum_value(value: Any) -> str:
    return str(getattr(value, "value", value))


def identity(client: Inkbox) -> tuple[Any, str]:
    mailboxes = client.mailboxes.list()
    if len(mailboxes) != 1:
        raise RuntimeError("Live A2A credentials must resolve to exactly one mailbox")
    handle = mailboxes[0].email_address.split("@", 1)[0]
    return client.get_identity(handle), handle


def parts_text(parts: list[dict[str, Any]]) -> str:
    return "\n".join(
        str(part["text"])
        for part in parts
        if isinstance(part, dict) and part.get("text") is not None
    )


def history_messages(task: Any) -> list[str]:
    return [
        parts_text(message.get("parts", []))
        for message in task.raw.get("history", [])
        if isinstance(message, dict)
    ]


def worker_history_messages(task: Any) -> list[str]:
    return [
        parts_text(message.get("parts", []))
        for message in task.raw.get("history", [])
        if isinstance(message, dict)
        and enum_value(message.get("role")) == "ROLE_AGENT"
    ]


def cancel_if_open(a2a: Any, target: Any, task_id: str) -> None:
    try:
        if enum_value(a2a.get_task(target, task_id).state) not in STOPPED_STATES:
            a2a.cancel(target, task_id)
    except Exception:
        pass


def main() -> None:
    timeout = float(os.environ.get("A2A_TIMEOUT_S", "210"))
    base_url = os.environ.get("INKBOX_BASE_URL", "https://inkbox.ai").rstrip("/")
    worker = Inkbox(api_key=required_env("ZEROCLAW_INKBOX_API_KEY"), base_url=base_url)
    requester = Inkbox(api_key=required_env("REMOTE_INKBOX_API_KEY"), base_url=base_url)
    _, worker_handle = identity(worker)
    requester_identity, _ = identity(requester)
    a2a = requester_identity.a2a_client()
    target = a2a.fetch_card(f"{base_url}/a2a/{worker_handle}/card")
    token = f"a2a-ci-inbound-progress-{uuid.uuid4().hex[:12]}"
    started = time.monotonic()
    result = a2a.send(
        target,
        text=(
            "Add 2 + 2. Wait for one minute. Then add 3 + 3. Wait for another "
            "minute. Finally add the two results together and return the final "
            f"total. Do not finish before both waits elapse. Include `{token}` "
            "and the exact expression `4 + 6 = 10` in the final answer."
        ),
        message_id=str(uuid.uuid4()),
    )
    if result.kind != "task" or result.task is None:
        raise AssertionError("A2A send did not return a task")
    task = result.task
    try:
        deadline = started + timeout
        acknowledgement = None
        completed = None
        while time.monotonic() < deadline:
            current = a2a.get_task(target, task.id, history_length=50)
            history = history_messages(current)
            observed = next(
                (
                    text
                    for text in history
                    if text.startswith(f"Task {task.id} received.")
                ),
                None,
            )
            if acknowledgement is None and observed is not None:
                acknowledgement = observed
                if time.monotonic() - started > 30:
                    raise AssertionError("Initial A2A acknowledgement was not prompt")
            state = enum_value(current.state)
            if state == "TASK_STATE_COMPLETED":
                completed = current
                break
            if state in STOPPED_STATES:
                raise AssertionError(f"A2A task stopped in unexpected state {state}")
            time.sleep(1)
        if acknowledgement is None:
            raise AssertionError("Initial A2A acknowledgement was not observed")
        if not acknowledgement.endswith(ACK_SUFFIX):
            raise AssertionError("Acknowledgement omitted the one-minute frequency")
        if completed is None:
            raise TimeoutError("A2A task did not complete before timeout")

        history = history_messages(completed)
        updates = [
            (index, match)
            for index, text in enumerate(history)
            if (match := PROGRESS.fullmatch(text)) is not None
        ]
        if len(updates) < 2:
            raise AssertionError(
                f"Expected at least two progress updates, got {len(updates)}"
            )
        elapsed = [int(match.group(1)) for _, match in updates]
        if not (50 <= elapsed[0] <= 90 and 50 <= elapsed[1] - elapsed[0] <= 90):
            raise AssertionError(f"Progress cadence was outside tolerance: {elapsed[:2]}")
        ack_index = history.index(acknowledgement)
        if not ack_index < updates[0][0] < updates[1][0]:
            raise AssertionError("Acknowledgement and progress messages are out of order")
        worker_messages = worker_history_messages(completed)
        if not worker_messages:
            raise AssertionError("Completed task has no worker-authored final message")
        final = worker_messages[-1]
        if token not in final or "4 + 6 = 10" not in final:
            raise AssertionError("Long-running A2A task returned the wrong result")
    finally:
        cancel_if_open(a2a, target, task.id)
        a2a.close()


if __name__ == "__main__":
    main()
