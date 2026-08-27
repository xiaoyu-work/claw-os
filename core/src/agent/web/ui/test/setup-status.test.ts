import { describe, expect, test } from "bun:test";

import { readSetupStatus } from "../src/lib/setup-status";

describe("readSetupStatus", () => {
  test("distinguishes a saved configuration from current readiness", () => {
    expect(
      readSetupStatus({
        configured: true,
        ready: false,
        provider: "copilot",
        reason: {
          error: "credential missing",
          details: "GitHub sign-in could not be resolved.",
          fix: "cos agent setup text",
        },
      }),
    ).toEqual({
      configured: true,
      ready: false,
      reason:
        "credential missing — GitHub sign-in could not be resolved. — Fix: cos agent setup text",
    });
  });

  test("infers configured state from older status responses", () => {
    expect(
      readSetupStatus({ ready: false, provider: "copilot" }).configured,
    ).toBe(true);
    expect(
      readSetupStatus({ ready: false, provider: "none" }).configured,
    ).toBe(false);
  });
});
