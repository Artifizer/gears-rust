"""Readable assertions on RFC 9457 problem responses.

Every failed assertion prints the request, the status, and the whole response
body, so a test failure says exactly what the server answered.
"""

import json

import httpx

ERR_PREFIX = "gts://gts.cf.core.errors.err.v1~cf.core.err."
RESOURCE_TYPE = "gts.x.object_storage.storage.object.v1~"


def show(r: httpx.Response) -> str:
    try:
        body = json.dumps(r.json(), indent=2, sort_keys=True)
    except ValueError:
        body = r.text
    return f"\n{r.request.method} {r.request.url}\n-> HTTP {r.status_code}\n{body}"


def expect_status(r: httpx.Response, status: int) -> dict:
    assert r.status_code == status, f"expected HTTP {status}{show(r)}"
    return r.json() if r.content else {}


def expect_problem(r: httpx.Response, status: int, category: str) -> dict:
    """A canonical error: status, category, and the gear's resource type."""
    body = expect_status(r, status)
    assert body.get("type") == f"{ERR_PREFIX}{category}.v1~", f"expected category `{category}`{show(r)}"
    assert body.get("status") == status, f"`status` in the body differs{show(r)}"
    assert body.get("instance") == r.request.url.path, f"`instance` is not the request path{show(r)}"
    assert body.get("context", {}).get("resource_type") == RESOURCE_TYPE, f"wrong resource_type{show(r)}"
    return body


def expect_field_violation(r: httpx.Response, field: str, description: str) -> None:
    """400 invalid_argument naming exactly one bad field."""
    body = expect_problem(r, 400, "invalid_argument")
    violations = body["context"].get("field_violations", [])
    assert violations == [
        {"field": field, "description": description, "reason": "VALIDATION_ERROR"}
    ], f"expected one violation on `{field}`{show(r)}"


def expect_not_found(r: httpx.Response, object_id: str) -> None:
    body = expect_problem(r, 404, "not_found")
    assert body["context"].get("resource_name") == object_id, f"404 names another resource{show(r)}"


def expect_blocked(r: httpx.Response, policy: str, message: str) -> None:
    """403 from a guardrail policy: the reason carries the policy and its message."""
    body = expect_problem(r, 403, "permission_denied")
    expected = f"GOVERNANCE_BLOCKED: {policy}: {message}"
    assert body["context"].get("reason") == expected, f"expected reason `{expected}`{show(r)}"


def expect_approval_required(r: httpx.Response, policy: str, message: str) -> None:
    """400 failed_precondition from an approval policy."""
    body = expect_problem(r, 400, "failed_precondition")
    violations = body["context"].get("violations", [])
    assert any(
        v.get("type") == "APPROVAL_REQUIRED" and f"{policy}: {message}" in v.get("description", "")
        for v in violations
    ), f"expected APPROVAL_REQUIRED from `{policy}`{show(r)}"
