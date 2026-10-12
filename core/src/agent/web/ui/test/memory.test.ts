import { expect, test } from "bun:test";
import { readMemoryEntry, readMemorySources } from "../src/lib/memory";

test("memory entries retain revisions and immutable source permissions", () => {
  const entry = readMemoryEntry({
    reference:{kind:"message",id:"9007199254740993"}, source:"conversation/session-1",
    category:"user", recorded_at_ms:1, revision:"0".repeat(64), content:"remembered interaction",
    editable:false, deletable:false, provenance:"Recorded interaction; not current truth.",
  });
  expect(entry.reference).toEqual({kind:"message",id:"9007199254740993"});
  expect(entry.editable).toBe(false);
  expect(entry.deletable).toBe(false);
  expect(() => readMemoryEntry({...entry,revision:"not-a-revision"})).toThrow();
});

test("retrieved memory references exclude actual memory content", () => {
  const sources = readMemorySources([{
    source:"notes/USER.md",revision:"0".repeat(64),reference:{kind:"note",name:"USER.md"},content:"PRIVATE",
  }]);
  expect(sources[0].reference).toEqual({kind:"note",name:"USER.md"});
  expect(JSON.stringify(sources)).not.toContain("PRIVATE");
});
