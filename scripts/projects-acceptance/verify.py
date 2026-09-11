"""Local fixture HTTP acceptance. Credentials remain in this process, never output."""

import argparse
import http.cookiejar
import json
import re
import urllib.error
import urllib.parse
import urllib.request
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--receipt", required=True)
args = parser.parse_args()
with open(args.receipt) as receipt_file:
    receipt = json.load(receipt_file)
origin = receipt["origin"]
assert origin == "http://127.0.0.1:55440", (
    "only the explicit local fixture is supported"
)
org = receipt["organization_id"]
run_id = uuid.uuid4().hex


def call(path, body=None, headers=None, client=None):
    r = urllib.request.Request(origin + path, data=body, headers=headers or {})
    try:
        with (client or urllib.request.build_opener()).open(r, timeout=30) as x:
            return x.status, x.read()
    except urllib.error.HTTPError as x:
        return x.code, x.read()


def jsoncall(path, p=None, token=None):
    status, data = call(
        path,
        None if p is None else json.dumps(p).encode(),
        {
            "Content-Type": "application/json",
            **({"Authorization": "Bearer " + token} if token else {}),
        },
    )
    if token:
        assert token.encode() not in data
    return status, json.loads(data)


def login(user):
    jar = http.cookiejar.CookieJar()
    client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(jar))
    status, _ = call(
        "/login",
        urllib.parse.urlencode(
            {
                "identifier": user + "@example.test",
                "password": "Local-acceptance-only-2026!",
            }
        ).encode(),
        {"Origin": origin, "Content-Type": "application/x-www-form-urlencoded"},
        client,
    )
    assert status == 200
    parent = next(x.value for x in jar if x.name == "acceptance_session")
    _, begin = jsoncall("/auth/agent/connection/begin", {})
    _, page = call(
        "/auth/agent/authorize?attempt=" + urllib.parse.quote(begin["attempt_id"]),
        client=client,
    )
    nonce = re.search(b'name="consent" value="([^"]+)"', page).group(1).decode()
    fields = urllib.parse.urlencode(
        {"attempt": begin["attempt_id"], "consent": nonce}
    ).encode()
    status, _ = call(
        "/auth/agent/approve",
        fields,
        {
            "Origin": "http://attacker.invalid",
            "Content-Type": "application/x-www-form-urlencoded",
        },
        client,
    )
    assert status == 403
    status, _ = call(
        "/auth/agent/approve",
        fields,
        {"Origin": origin, "Content-Type": "application/x-www-form-urlencoded"},
        client,
    )
    assert status == 200
    status, _ = call(
        "/auth/agent/approve",
        fields,
        {"Origin": origin, "Content-Type": "application/x-www-form-urlencoded"},
        client,
    )
    assert status == 403
    _, poll = jsoncall(
        "/auth/agent/connection/poll",
        {"attempt_id": begin["attempt_id"], "polling_secret": begin["polling_secret"]},
    )
    child = poll["grant"]["credential"]
    assert child != parent
    return client, parent, child


def tool(token, name, args):
    return jsoncall(
        "/projects/agent/tools/execute",
        {"name": name, "arguments_json": json.dumps(args)},
        token,
    )


def read(token, issue="issue-private", organization=org):
    status, response = tool(
        token,
        "projects_get_issue",
        {"organization_id": organization, "issue_ref": issue},
    )
    return status, json.loads(response["content"]) if status == 200 else response


alice, aparent, a = login("alice")
bob, bparent, b = login("bob")
assert jsoncall("/projects/agent/manifest")[0] == 200
assert jsoncall("/projects/agent/tools")[0] == 401
assert read(None)[0] == 401 and read("invalid-session")[0] == 401
assert read(b, "issue-public")[0] == 200 and read(b)[0] == 403
assert read(a, organization=receipt["other_organization_id"])[0] == 403
# Assignment is part of the same revision sequence as other Issue edits.
_, public_issue = read(a, "issue-public")
assignment = {
    "organization_id": org,
    "issue_id": "issue-public",
    "assignee_subject": receipt["alice_subject"],
    "expected_revision": public_issue["revision"],
    "idempotency_key": "protocol-assignment-" + run_id,
}
assigned = tool(a, "projects_set_issue_assignee", assignment)
assert assigned[0] == 200, assigned
assert tool(a, "projects_set_issue_assignee", assignment) == assigned
_, current = read(a, "issue-public")
assert int(current["revision"]) == int(public_issue["revision"]) + 1
for subject in ["not-an-organization-member"]:
    assert tool(a, "projects_set_issue_assignee", {
        **assignment, "assignee_subject": subject,
        "expected_revision": current["revision"],
        "idempotency_key": "protocol-nonmember-" + run_id,
    })[0] == 403
assert read(a, "issue-public")[1] == current
cleared = tool(a, "projects_set_issue_assignee", {
    **assignment, "assignee_subject": None,
    "expected_revision": current["revision"],
    "idempotency_key": "protocol-clear-" + run_id,
})
assert cleared[0] == 200, cleared
status, assignee = tool(a, "projects_get_issue_assignee", {
    "organization_id": org, "issue_id": "issue-public",
})
assert status == 200 and json.loads(assignee["content"])["assignee_subject"] is None

status, original = read(a)
assert status == 200
keys = [
    "organization_id",
    "issue_id",
    "title",
    "description",
    "priority",
    "workflow_state_id",
    "cycle_id",
    "milestone_id",
    "parent_issue_id",
    "label_ids",
]
update = {k: original[k] for k in keys}
update.update(
    idempotency_key="protocol-private-update-1-" + run_id,
    expected_revision=original["revision"],
    title="Protocol acceptance passed",
)
status, result = tool(a, "projects_update_issue", update)
assert status == 200, (status, result)
assert tool(a, "projects_update_issue", update) == (status, result), (
    "identical retry must replay"
)
_, updated = read(a)
assert int(updated["revision"]) == int(original["revision"]) + 1
stale = {**update, "idempotency_key": "protocol-stale-update-" + run_id}
status, error = tool(a, "projects_update_issue", stale)
assert status == 422 and error["payload"]["reason_code"] == "revision_conflict", (
    status,
    error,
)
denied = {
    **update,
    "idempotency_key": "bob-private-denied-" + run_id,
    "expected_revision": updated["revision"],
}
assert tool(b, "projects_update_issue", denied)[0] == 403
assert read(a)[1] == updated, "denied and stale writes must not change the issue"
create = {
    k: updated[k]
    for k in [
        "organization_id",
        "project_id",
        "team_id",
        "title",
        "description",
        "priority",
        "workflow_state_id",
        "cycle_id",
        "milestone_id",
        "parent_issue_id",
        "label_ids",
    ]
}
create.update(idempotency_key="outside-grant-" + run_id, issue_id="must-not-exist")
assert tool(a, "projects_create_issue", create)[0] == 403, (
    "Tool Provider forwarding must not widen the final operation audience"
)
status, _ = call("/logout", b"", {"Origin": origin}, alice)
assert status == 200
assert read(a)[0] == 401 and read(aparent)[0] == 401, (
    "parent revocation must revoke its child"
)
assert read(b, "issue-public")[0] == 200, (
    "revocation must not revoke a different account"
)
print(
    json.dumps(
        {
            "checks": [
                "browser-consent-protocol",
                "wrong-origin",
                "consent-replay",
                "public-manifest",
                "anonymous-denied",
                "invalid-session",
                "private-team-read-denied",
                "nonmember-scope-denied",
                "revision-update",
                "assignment-idempotency",
                "nonmember-assignment-denied",
                "clear-assignment",
                "idempotent-replay",
                "revision-conflict",
                "private-team-write-denied",
                "no-mutation-on-denial",
                "operation-audience-narrowing",
                "parent-revocation",
                "account-isolation",
            ],
            "organization_id": org,
            "private_revision": updated["revision"],
            "passed": True,
        }
    )
)
