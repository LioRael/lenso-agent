import { AgentError, jsonCopy } from "./safety";
import type { AgentEvent, EventKind } from "./types";

type Details = { callId?: string; actionId?: string; data?: unknown };
type Subscriber = {
  queue: AgentEvent[];
  waiting?: (result: IteratorResult<AgentEvent>) => void;
  ended: boolean;
};

/** Holds only an incomplete secret prefix, including prefixes split across provider chunks. */
export class SafeText {
  private pending = "";

  constructor(private readonly secrets: () => readonly string[]) {}

  push(text: string, flush = false): string {
    this.pending += text;
    const secrets = this.secrets().filter(Boolean).sort((a, b) => b.length - a.length);
    let output = "";
    while (this.pending) {
      const match = secrets.find(secret => this.pending.startsWith(secret));
      if (match) {
        output += "[redacted]";
        this.pending = this.pending.slice(match.length);
      } else if (!flush && secrets.some(secret => secret.startsWith(this.pending))) {
        break;
      } else {
        const character = String.fromCodePoint(this.pending.codePointAt(0)!);
        if (!flush && character.length === 1 && /[\uD800-\uDBFF]/.test(character) && this.pending.length === 1) break;
        output += character;
        this.pending = this.pending.slice(character.length);
      }
    }
    return output;
  }
}

/** A bounded live fan-out. Detaching a consumer never touches execution. */
export class RunEvents {
  private currentSequence = 0;
  private closed = false;
  private readonly subscribers = new Set<Subscriber>();

  constructor(
    private readonly sessionId: string, private readonly runId: string,
    private readonly capacity: number, private readonly maxBytes = 1024 * 1024,
  ) {}

  get sequence(): number { return this.currentSequence; }

  emit = (kind: EventKind, details: Details = {}): void => {
    if (this.closed) return;
    const event: AgentEvent = {
      ...details, id: crypto.randomUUID(), sequence: ++this.currentSequence,
      timestamp: Date.now(), sessionId: this.sessionId, runId: this.runId, kind,
    };
    for (const subscriber of this.subscribers) {
      if (subscriber.waiting) {
        const waiting = subscriber.waiting;
        subscriber.waiting = undefined;
        waiting({ done: false, value: jsonCopy(event, this.maxBytes) });
      } else if (subscriber.queue.length < this.capacity) {
        subscriber.queue.push(jsonCopy(event, this.maxBytes));
      } else {
        subscriber.queue = [{ ...event, kind: "safe_error", data: { code: "event-overflow" }, callId: undefined, actionId: undefined }];
        subscriber.ended = true;
        this.subscribers.delete(subscriber);
      }
    }
  };

  subscribe(): AsyncIterable<AgentEvent> {
    if (!this.closed && this.subscribers.size >= this.capacity) throw new AgentError("too-many-subscribers");
    const subscriber: Subscriber = { queue: [], ended: this.closed };
    if (!this.closed) this.subscribers.add(subscriber);
    const detach = () => {
      subscriber.ended = true;
      subscriber.queue = [];
      this.subscribers.delete(subscriber);
      subscriber.waiting?.({ done: true, value: undefined });
      subscriber.waiting = undefined;
    };
    const iterator: AsyncIterableIterator<AgentEvent> = {
      [Symbol.asyncIterator]() { return this; },
      next: () => {
        const value = subscriber.queue.shift();
        if (value) return Promise.resolve({ done: false, value });
        if (subscriber.ended) return Promise.resolve({ done: true, value: undefined });
        if (subscriber.waiting) {
          detach();
          return Promise.reject(new AgentError("concurrent-event-read"));
        }
        return new Promise(resolve => { subscriber.waiting = resolve; });
      },
      return: async () => { detach(); return { done: true, value: undefined }; },
      throw: async () => { detach(); throw new AgentError("event-stream-closed"); },
    };
    return iterator;
  }

  close(): void {
    this.closed = true;
    for (const subscriber of this.subscribers) {
      subscriber.ended = true;
      subscriber.waiting?.({ done: true, value: undefined });
      subscriber.waiting = undefined;
    }
    this.subscribers.clear();
  }
}
