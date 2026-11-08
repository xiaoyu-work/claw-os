import { stringifyWireJson } from "./generated";
import { cosCallJson } from "./transport";

/** Root derives the App/owner and resolves reply recipients from verified events. */
export function control(
  action: "status" | "start" | "stop" | "reply",
  options: { eventId?: string; text?: string; confirm?: boolean } = {},
): unknown {
  const input = stringifyWireJson({
    action,
    ...(options.eventId === undefined ? {} : { event_id: options.eventId }),
    ...(options.text === undefined ? {} : { text: options.text }),
    ...(options.confirm === undefined ? {} : { confirm: options.confirm }),
  });
  if (Buffer.byteLength(input, "utf8") > 20 * 1024) {
    throw new Error("channel control exceeds 20 KiB");
  }
  return cosCallJson("channel control", ["__channel", "control"], 30_000, input);
}
