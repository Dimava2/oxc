import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join, relative } from "node:path";
import prettier from "prettier";
import { describe, expect, it } from "vitest";
import { format } from "../../dist/index.js";

// NOTE: Fixtures can be downloaded by `pnpm download-prettier-fixtures`
const FIXTURES_DIR = join(import.meta.dirname, "../../prettier-fixtures");
const MAX_INTERNAL_FIXTURES = 25;

describe("experimentalVueInternal differential report", () => {
  const vueFixtures = collectFixtures(".vue", [
    "vue/range/example.vue",
    "vue/multiparser/lang-tsx.vue",
  ])
    .filter(({ name }) => name.startsWith("vue/"))
    .slice(0, MAX_INTERNAL_FIXTURES);

  vueFixtures.push(
    {
      name: "edge/template-interpolation-spacing.vue",
      content: `<template>   <div>{{answer}}</div> </template>\n`,
    },
    {
      name: "edge/template-interpolation-multiline.vue",
      content: `<template>\n  <Comp :label="\`\${ foo }\`">{{msg}}</Comp>\n</template>\n`,
    },
    {
      name: "edge/script-and-template.vue",
      content: `<script setup lang="ts">\nimport z from "z"\nimport a from "a"\nconst answer=1\n</script>\n<template>   <div>{{answer}}</div> </template>\n`,
    },
  );

  it.each([
    { printWidth: 80 },
    {
      printWidth: 100,
      vueIndentScriptAndStyle: true,
      singleQuote: true,
    },
  ])("produces differential summary %j", async (options) => {
    const records = await Promise.all(
      vueFixtures.map(async ({ name, content }) => {
        const [internalResult, prettierResult] = await compareWithPrettierUsingInternalMode(
          name,
          content,
          options,
        );
        return {
          name,
          internalResult,
          prettierResult,
          equalsPrettier: internalResult === prettierResult,
          changedFromInput: internalResult !== content,
          isError: internalResult === "ERROR",
        };
      }),
    );

    const mismatches = records.filter((record) => !record.equalsPrettier).map((record) => record.name);
    const errors = records.filter((record) => record.isError).map((record) => record.name);
    const changedFromInput = records.filter((record) => record.changedFromInput).length;

    const summary = {
      fixtures: records.length,
      errors: errors.length,
      equalToPrettier: records.length - mismatches.length,
      mismatches: mismatches.length,
      changedFromInput,
      mismatchSample: mismatches.slice(0, 10),
      errorSample: errors.slice(0, 10),
    };

    expect(summary.errors).toBe(0);
    expect(summary).toMatchSnapshot();
  });
});

// ---

type TestCase = { name: string; content: string };

function collectFixtures(ext: string, excludes: string[] = []): TestCase[] {
  const dir = FIXTURES_DIR;
  // NOTE: In CI, the fixtures might not be present, just skip and only run edge cases.
  if (!existsSync(dir)) return [];

  const results: TestCase[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true, recursive: true })) {
    if (!entry.isFile() || !entry.name.endsWith(ext)) continue;

    const fullPath = join(entry.parentPath, entry.name);
    const relPath = relative(dir, fullPath);
    if (excludes.some((value) => relPath.includes(value))) continue;

    results.push({ name: relPath, content: readFileSync(fullPath, "utf8") });
  }

  return results.sort((a, b) => a.name.localeCompare(b.name));
}

async function compareWithPrettierUsingInternalMode(
  fileName: string,
  content: string,
  options = {},
) {
  let prettierResult;
  try {
    prettierResult = await prettier.format(content, {
      parser: "vue",
      filepath: fileName,
      ...options,
    });
  } catch {
    prettierResult = "ERROR";
  }

  let internalResult;
  const internalResponse = await format(fileName, content, {
    ...options,
    experimentalVueInternal: true,
  });
  if (internalResponse.errors.length !== 0) {
    internalResult = "ERROR";
  } else {
    internalResult = internalResponse.code;
  }

  return [internalResult, prettierResult];
}
