"""API and governance tests for the object-storage example.

They run against the policies in ../config/object-storage.yaml:

- no-executables (guardrail, create): blocks `application/x-msdownload`.
- large-objects (approval, create): objects over 10 MB need approval.
- confidential-owner-only (guardrail, get): confidential objects are readable
  by their owner only.
- retention (guardrail, delete): objects younger than one hour cannot be deleted.

Every error is checked in full (status, category, resource, and the details)
through the helpers in problems.py, which print the whole response on failure.
"""

import uuid

from conftest import OBJECTS
from problems import (
    expect_blocked,
    expect_field_violation,
    expect_not_found,
    expect_problem,
    expect_status,
)

MAX_BODY_BYTES = 1024 * 1024  # `max_body_bytes` in ../config/object-storage.yaml


# --- the API -----------------------------------------------------------------


def test_create_generates_size_and_timestamps(create):
    obj = expect_status(create(body="hello"), 201)
    assert obj["size_bytes"] == 5, obj
    assert obj["created_at"] == obj["updated_at"], obj
    assert obj["tenant_id"] and obj["owner_id"], obj


def test_get_returns_the_created_object(client, create):
    obj = expect_status(create(), 201)
    assert expect_status(client.get(f"{OBJECTS}/{obj['id']}"), 200) == obj


def test_patch_changes_body_and_size(client, create):
    obj = expect_status(create(body="hi"), 201)
    changed = expect_status(client.patch(f"{OBJECTS}/{obj['id']}", json={"body": "hello world"}), 200)
    assert changed["size_bytes"] == 11, changed
    assert changed["updated_at"] >= obj["updated_at"], changed
    assert changed["created_at"] == obj["created_at"], "created_at must not change"


# --- validation errors (before authorization and governance) -----------------


def test_unknown_object_is_not_found(client):
    missing = str(uuid.uuid4())
    expect_not_found(client.get(f"{OBJECTS}/{missing}"), missing)


def test_invalid_content_type_names_the_field(create):
    expect_field_violation(
        create(content_type="not-a-mime-type"),
        "content_type",
        "must be a MIME type such as text/plain",
    )


def test_blank_name_names_the_field(create):
    expect_field_violation(create(name=" "), "name", "must be 1-255 characters")


def test_malformed_id_is_a_bad_request(client):
    r = client.get(f"{OBJECTS}/not-a-uuid")
    expect_status(r, 400)


def test_wrong_json_type_is_unprocessable(client):
    r = client.post(OBJECTS, json={"name": 1, "content_type": "text/plain"})
    expect_status(r, 422)


# --- guardrails ----------------------------------------------------------------


def test_executable_is_blocked_with_the_policy_message(create):
    expect_blocked(
        create(content_type="application/x-msdownload", body="MZ"),
        "no-executables",
        "executables cannot be stored",
    )


def test_retention_blocks_deleting_a_fresh_object(client, create):
    obj = expect_status(create(), 201)
    expect_blocked(
        client.delete(f"{OBJECTS}/{obj['id']}"),
        "retention",
        "objects younger than one hour cannot be deleted",
    )
    # Nothing was deleted.
    expect_status(client.get(f"{OBJECTS}/{obj['id']}"), 200)


def test_deleting_a_missing_object_is_not_found_not_a_refusal(client):
    # The retention policy reads the object's age; the gear reads the object
    # only for it, finds nothing, and answers 404 instead of 403.
    missing = str(uuid.uuid4())
    expect_not_found(client.delete(f"{OBJECTS}/{missing}"), missing)


def test_owner_can_read_a_confidential_object(client, create):
    # With static auth every request is the same user, so the caller is the
    # owner and the policy allows the read.
    obj = expect_status(create(labels={"classification": "confidential"}), 201)
    expect_status(client.get(f"{OBJECTS}/{obj['id']}"), 200)


def test_blocked_create_writes_nothing(client, create):
    name = f"blocked-{uuid.uuid4().hex[:8]}"
    expect_problem(create(name=name, content_type="application/x-msdownload"), 403, "permission_denied")
    # The gear has no list endpoint; a refused create returns no id to fetch,
    # and the next create with the same name succeeds as a new object.
    expect_status(create(name=name), 201)


# --- approval ------------------------------------------------------------------


def test_body_above_the_gear_limit_never_reaches_the_approval_policy(create):
    # The example config caps bodies at 1 MB, below the 10 MB threshold of
    # `large-objects`, so validation refuses first. Raise `max_body_bytes`
    # above 10 MB (or lower the policy's threshold) and switch this test to
    # problems.expect_approval_required(r, "large-objects",
    # "objects over 10 MB need approval").
    expect_field_violation(
        create(body="x" * (MAX_BODY_BYTES + 1)),
        "body",
        f"must be at most {MAX_BODY_BYTES} bytes",
    )
