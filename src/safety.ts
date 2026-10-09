import { boundedJson } from "@lenso/engine/operations";
import { redact } from "@lenso/engine/diagnostics";
import type { Identity } from "./types";

export class AgentError extends Error {
  constructor(readonly code: string) {
    super(code);
    this.name = "AgentError";
  }
}
export function sameIdentity(left: Identity, right: Identity): boolean {
  return left.subject === right.subject && left.scope === right.scope &&
    left.application === right.application && left.target === right.target;
}
export function jsonCopy<T>(value: T, maxBytes = 1024 * 1024): T {
  return JSON.parse(boundedJson(value, maxBytes)) as T;
}
export function safeValue<T>(value: T, secrets: readonly string[]): T {
  const json = JSON.stringify(value, (key, item) => {
    // Omit unsafe names instead of renaming keys and accidentally merging distinct fields.
    if (key && ((redact(key, secrets) as string) !== key ||
      secrets.some((secret) => secret && key.includes(secret)))) return undefined;
    if (/^(password|passwd|secret|token|api[-_]?key|access[-_]?token|refresh[-_]?token|authorization|cookie|credential|connection[-_]?string)s?$/i.test(key))
      return "[redacted]";
    if (typeof item === "string") {
      item = (redact(item, secrets) as string).replaceAll("[REDACTED]", "[redacted]");
      for (const secret of secrets) if (secret) item = item.split(secret).join("[redacted]");
    }
    return item;
  });
  if (json === undefined) throw new AgentError("invalid-json");
  return JSON.parse(json) as T;
}
export function assertSafeInput(value: unknown, secrets: readonly string[], maxBytes: number): void {
  const raw = boundedJson(value, maxBytes);
  if (raw !== boundedJson(safeValue(value, secrets), maxBytes))
    throw new AgentError("sensitive-input");
}
export function safeCode(error: unknown): string {
  if (error instanceof AgentError) return error.code;
  // Domain messages, causes and provider errors may contain credentials.
  const diagnostic = error && typeof error === "object" ? Reflect.get(error, "diagnostic") : undefined;
  const code = diagnostic && typeof diagnostic === "object" ? Reflect.get(diagnostic, "code")
    : error && typeof error === "object" ? Reflect.get(error, "code") : undefined;
  return ["invalid-input", "forbidden-operation", "confirmation-required", "approval-required", "unknown-operation"].includes(code)
    ? code : "operation-failed";
}
