# Publish only an approved revision

Fix the proposal publisher so it creates and applies revision 2 only when the
approval is for that exact revision. An attempt using approval revision 1 must
be rejected as stale and must not create a second side effect.
