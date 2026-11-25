"""Fixtures for the object-storage API and governance tests.

Start the server first (see ../README.md), then run `pytest`. Every test is
skipped when the server is not reachable.
"""

import os
import uuid

import httpx
import pytest

BASE_URL = os.getenv("OBJECT_STORAGE_URL", "http://127.0.0.1:8090")
OBJECTS = "/object-storage/v1/objects"


@pytest.fixture(scope="session")
def client():
    with httpx.Client(base_url=BASE_URL, timeout=10.0) as c:
        try:
            c.get("/health")
        except httpx.TransportError:
            pytest.skip(f"object-storage-server is not running at {BASE_URL}")
        yield c


@pytest.fixture
def create(client):
    """Create an object and return the response."""

    def _create(content_type="text/plain", body="hello", labels=None, name=None):
        return client.post(
            OBJECTS,
            json={
                "name": name or f"obj-{uuid.uuid4().hex[:8]}",
                "content_type": content_type,
                "body": body,
                "labels": labels or {},
            },
        )

    return _create
