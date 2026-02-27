import { describe, expect, it } from "vitest";
import { join } from "node:path";
import { runWriteModeAndSnapshot } from "../utils";

const fixturesDir = join(import.meta.dirname, "fixtures");

describe("vue_internal", () => {
  it("should format script blocks when internal vue mode is enabled", async () => {
    const snapshot = await runWriteModeAndSnapshot(fixturesDir, ["input.vue"]);
    expect(snapshot).toMatchSnapshot();
  });

  it("should format template-only files when internal vue mode is enabled", async () => {
    const snapshot = await runWriteModeAndSnapshot(fixturesDir, ["template-only.vue"]);
    expect(snapshot).toMatchSnapshot();
  });
});
