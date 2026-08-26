# Subagent Plugin

This reviewed native Plugin adds one `delegate` Tool to the root Agent. The
Tool invokes the separately composed `subagent-agent` Instance and returns its
text plus the durable child Session ID.

The child Agent is not a recursive copy of the root authority. The base App
binds it to `subagent-tools`, a narrow Tools Module that can only call the
reviewed `workspace-import-read` Capability. Enabling workspace mutation or
process execution for the root Agent therefore does not grant either behavior
to the child.

The first version intentionally admits one child Turn at a time. A later pool
can add parallel child Agent Instances without changing the Kernel or the Tool
Provider contract.
