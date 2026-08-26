# Code Mode Plugin

This reviewed bundled Plugin contributes the exclusive `run_code` Tool. The
Tool evaluates bounded Lua 5.4 source and can invoke only the separate
`restricted-read-tools` Runtime selected by the Host. The first profile exposes
only `read_text`; enabling other root Tool Plugins does not widen Code Mode.

Example Tool arguments:

```json
{
  "code": "local values = parallel({{name='read_text', arguments={path='README.md'}}, {name='read_text', arguments={path='README.md'}}}); return {same=values[1] == values[2], first=values[1]}"
}
```

The interpreter has no filesystem, process, network, `io`, `os`, `package`, or
`debug` library. It is still an in-process interpreter, not a hostile-code
security sandbox.
