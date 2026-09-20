# Normalize an operator supplied port

Fix the regression that rejects a valid port when the operator includes leading
or trailing whitespace. Preserve the zero-port validation and do not modify
unrelated files.
