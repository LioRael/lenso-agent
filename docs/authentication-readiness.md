# Authentication and Console readiness

Missing or rejected ChatGPT credentials do not prevent the Auth and Model Plugins from activating. The direct Model Plugin remains unavailable for inference until authentication succeeds and a live account catalog is loaded. A cancellable, Generation-owned background task waits for login and publishes the account-scoped catalog using the existing publisher fence. It does not borrow a different account's cache.

Settings and Web bootstrap read the active Generation's bound Tool catalog directly. They do not lease an inference Turn or require a model catalog. Actual Turns retain model readiness and immutable Profile checks. This keeps Connections available so users can log in through Console.

Validation: model-plugin tests, Web library tests, and a real cold start without the Lenso credential file. The local Console login UI was checked through browser-attempt creation and cancellation without completing provider authentication.
