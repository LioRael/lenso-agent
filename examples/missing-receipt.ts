import type { AgentStore, PendingAction, StoreTransaction, ToolCall } from "../src/types";

/** Explicit offline fault: business SQL commits, then every terminal receipt for
 * one action fails. It is not a production store, crash detector or retry policy.
 */
export function missingReceiptStore(borrowed: AgentStore) {
  let blockedAction: string | undefined;
  const store: AgentStore = {
    transaction(work) {
      return borrowed.transaction(tx => {
        const fault: StoreTransaction = {
          get: tx.get.bind(tx),
          list: tx.list.bind(tx),
          count: tx.count.bind(tx),
          bytes: tx.bytes.bind(tx),
          page: tx.page.bind(tx),
          put(table, record) {
            const terminal = table === "actions"
              ? (record as PendingAction).id === blockedAction && !["pending", "executing"].includes((record as PendingAction).status)
              : table === "calls"
                ? (record as ToolCall).actionId === blockedAction && !["prepared", "awaiting_confirmation", "executing"].includes((record as ToolCall).status)
                : false;
            if (blockedAction && terminal) throw new Error("offline-injected-missing-receipt");
            tx.put(table, record);
          },
        };
        return work(fault);
      });
    },
    close() { /* The host owns borrowed storage. */ },
  };
  return { store, arm(actionId: string) { blockedAction = actionId; } };
}
