import { stringifyWireJson, type WireJsonValue } from "./generated";
import { cosCallJson } from "./transport";

/** Publish reported App data. Root derives owner, App and source from the live session. */
export function publish(
  eventType: string,
  key: string,
  options: { payload?: WireJsonValue; entityId?: string } = {},
): unknown {
  const input = stringifyWireJson({
    event_type: eventType,
    key,
    ...(options.payload === undefined ? {} : { payload: options.payload }),
    ...(options.entityId === undefined ? {} : { entity_id: options.entityId }),
  });
  if (Buffer.byteLength(input, "utf8") > 20 * 1024) {
    throw new Error("App event request exceeds 20 KiB");
  }
  return cosCallJson("App event publication", ["__app_event", "publish"], 30_000, input);
}
