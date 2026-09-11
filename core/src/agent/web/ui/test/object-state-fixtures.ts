import type { ObjectStateEntry } from "../src/lib/object-state";

export const OBJECT_REFERENCE = "app://archive/entry?id=release";
export const OBJECT_ENTRY_ID = "00000000-0000-4000-8000-000000000001";
export const OBJECT_NEXT_ID = "00000000-0000-4000-8000-000000000002";

export function objectStateFixture(id = OBJECT_ENTRY_ID, activityId = "activity-1"): ObjectStateEntry {
  return {
    id, activity_id: activityId, owner_uid: 1000, recorded_at: "2026-09-11T10:00:00Z",
    source: "caller_reported", superseded_by: null, validity: "unknown", receipt: null,
    draft: {
      id, reference: OBJECT_REFERENCE, observed_at: null, valid_until: null, supersedes: null,
      content: { kind: "user_statement", text: '<img src="https://state.invalid/a"> Waiting for review' },
    },
  };
}
